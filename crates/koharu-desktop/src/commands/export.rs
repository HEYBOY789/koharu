//! Export: every page as PNG or PSD files in a folder, or as a CBZ of PNGs.

use std::{
    io::{Cursor, Write as _},
    path::Path,
    sync::Arc,
};

use anyhow::Context as _;
use fs_err::File;
use image::ImageFormat;
use koharu_psd::Labels;
use koharu_renderer::{Frame, Renderer};
use koharu_scene::{PageId, Session, Wiki};
use parking_lot::Mutex;
use serde::Deserialize;
use specta::Type;
use tauri::{State, Window};
use tauri_runtime_cef::CefRuntime;

use super::Result;
use crate::app::App;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Png,
    Psd,
    Cbz,
}

/// Picks where the open project's pages go, and writes them there. `labels`
/// names a PSD's layers the scene names nothing, in the interface's language.
#[tauri::command]
#[specta::specta]
pub async fn export(
    app: State<'_, Arc<App>>,
    window: Window<CefRuntime>,
    format: ExportFormat,
    labels: Labels,
) -> Result<()> {
    let (name, session) = {
        let open = app.project.lock();
        let project = open.as_ref().context("no project is open")?;
        (project.name.clone(), Arc::clone(&project.session))
    };
    let dialog = rfd::AsyncFileDialog::new().set_parent(&window);
    let destination = match format {
        ExportFormat::Png | ExportFormat::Psd => dialog.pick_folder().await,
        ExportFormat::Cbz => {
            dialog
                .set_file_name(format!("{name}.cbz"))
                .add_filter("CBZ", &["cbz"])
                .save_file()
                .await
        }
    };
    let Some(destination) = destination.map(|picked| picked.path().to_owned()) else {
        return Ok(());
    };
    let renderer = app.renderer.clone();
    Ok(tokio::task::spawn_blocking(move || {
        let pages = session
            .lock()
            .project()
            .pages()
            .map(|page| page.id)
            .collect::<Vec<_>>();
        write(&session, &renderer, &pages, format, &labels, &destination)
    })
    .await??)
}

#[tauri::command]
#[specta::specta]
pub async fn export_chapters(
    app: State<'_, Arc<App>>,
    window: Window<CefRuntime>,
    format: ExportFormat,
    labels: Labels,
) -> Result<()> {
    let session = app.session()?;
    let Some(output) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .pick_folder()
        .await
    else {
        return Ok(());
    };
    let output = output.path().to_owned();
    let renderer = app.renderer.clone();
    Ok(tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let project = session.lock().project();
        let mut used = std::collections::BTreeSet::new();
        for chapter in &project.metadata().chapters.entries {
            if chapter.pages.is_empty() {
                continue;
            }
            let base = sanitize_filename::sanitize(&chapter.title);
            let base = if base.is_empty() {
                chapter.id.to_string()
            } else {
                base
            };
            let mut name = base.clone();
            let mut suffix = 2;
            while !used.insert(name.to_lowercase()) {
                name = format!("{base} ({suffix})");
                suffix += 1;
            }
            let destination = match format {
                ExportFormat::Cbz => output.join(format!("{name}.cbz")),
                ExportFormat::Png | ExportFormat::Psd => {
                    let path = output.join(name);
                    fs_err::create_dir_all(&path)?;
                    path
                }
            };
            let pages = project
                .pages()
                .filter(|page| chapter.pages.contains(&page.id))
                .map(|page| page.id)
                .collect::<Vec<_>>();
            write(&session, &renderer, &pages, format, &labels, &destination)?;
        }
        Ok(())
    })
    .await??)
}

#[tauri::command]
#[specta::specta]
pub async fn export_wiki(app: State<'_, Arc<App>>, window: Window<CefRuntime>) -> Result<()> {
    let session = app.session()?;
    let wiki = session.lock().project().metadata().wiki.clone();
    if wiki.files.iter().all(|file| file.body.trim().is_empty()) {
        return Err(anyhow::anyhow!("the wiki has no content to export").into());
    }
    let Some(output) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .pick_folder()
        .await
    else {
        return Ok(());
    };
    let output = output.path().to_owned();
    Ok(tokio::task::spawn_blocking(move || write_wiki(&wiki, &output)).await??)
}

/// Keep the same `wiki/` layout the importer accepts for a series folder.
pub(super) fn write_wiki(wiki: &Wiki, output: &Path) -> anyhow::Result<()> {
    let directory = output.join("wiki");
    fs_err::create_dir_all(&directory)?;
    for file in &wiki.files {
        if !file.body.trim().is_empty() {
            fs_err::write(directory.join(format!("{}.md", file.slug)), &file.body)?;
        }
    }
    Ok(())
}

pub(super) fn write(
    session: &Mutex<Session>,
    renderer: &Renderer,
    pages: &[PageId],
    format: ExportFormat,
    labels: &Labels,
    destination: &Path,
) -> anyhow::Result<()> {
    let digits = pages.len().to_string().len().max(3);
    let mut archive = match format {
        ExportFormat::Cbz => Some(::zip::ZipWriter::new(File::create(destination)?)),
        ExportFormat::Png | ExportFormat::Psd => None,
    };
    for (index, &page) in pages.iter().enumerate() {
        let number = format!("{:0digits$}", index + 1);
        match (format, &mut archive) {
            (ExportFormat::Psd, _) => {
                let psd = koharu_psd::export(&session.lock(), renderer, page, labels)?;
                fs_err::write(destination.join(format!("{number}.psd")), psd)?;
            }
            (_, Some(archive)) => {
                // Already-compressed PNGs gain nothing from deflate.
                let options = ::zip::write::SimpleFileOptions::default()
                    .compression_method(::zip::CompressionMethod::Stored);
                archive.start_file(format!("{number}.png"), options)?;
                archive.write_all(&png(&renderer.render(&session.lock(), page)?)?)?;
            }
            (_, None) => {
                fs_err::write(
                    destination.join(format!("{number}.png")),
                    png(&renderer.render(&session.lock(), page)?)?,
                )?;
            }
        }
    }
    if let Some(archive) = archive {
        archive.finish()?;
    }
    Ok(())
}

fn png(frame: &Frame) -> anyhow::Result<Vec<u8>> {
    let raster = frame.rasterize()?;
    let mut png = Cursor::new(Vec::new());
    raster.image.write_to(&mut png, ImageFormat::Png)?;
    Ok(png.into_inner())
}
