use anyhow::{Context as _, Result};
use image::{
    ExtendedColorType, ImageEncoder as _,
    codecs::png::{CompressionType, FilterType, PngEncoder},
};
use koharu_pipeline::StopToken;
use koharu_psd::{PsdExportOptions, export_page};
use koharu_rasterizer::{Raster, RasterOptions, Rasterizer};
use koharu_renderer::{Frame, Renderer};
use koharu_scene::{AssetRole, Chapters, EntityId, Snapshot};
use serde::Deserialize;
use specta::Type;
use std::{collections::HashSet, io::Write as _, path::PathBuf, sync::Arc};
use tauri::{State, WebviewWindow};
use tauri_runtime_cef::CefRuntime;

use super::{Error, ImageBytes, project::CurrentProject};
use koharu_desktop::Desktop;

const THUMBNAIL_EDGE: u32 = 128;

#[derive(Clone, Copy, Debug, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Png,
    Psd,
    Cbz,
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn export(
    window: WebviewWindow<CefRuntime>,
    format: ExportFormat,
    project: State<'_, CurrentProject>,
    desktop: State<'_, Desktop>,
) -> std::result::Result<(), Error> {
    let (name, snapshot) = {
        let project = project.project.lock().await;
        let project = project.as_ref().context("no project is open")?;
        (project.name.clone(), project.snapshot())
    };
    let pages = snapshot.pages().map(|page| page.id()).collect::<Vec<_>>();
    if pages.is_empty() {
        return Err(anyhow::anyhow!("there are no pages to export").into());
    }
    // A chapter run is one folder or archive per chapter, so every format picks
    // a destination folder instead of a single file.
    let Some(destination) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .pick_folder()
        .await
        .map(|destination| destination.path().to_owned())
    else {
        return Ok(());
    };
    export_snapshot(name, snapshot, format, destination, &desktop, None).await?;
    Ok(())
}

pub(crate) async fn export_snapshot(
    name: String,
    snapshot: Snapshot,
    format: ExportFormat,
    destination: PathBuf,
    desktop: &Desktop,
    stop: Option<&StopToken>,
) -> Result<()> {
    let pages = snapshot.pages().map(|page| page.id()).collect::<Vec<_>>();
    let groups = export_groups(&snapshot.chapters()?, &pages);
    let renderer = desktop.renderer();
    let rasterizer = desktop.rasterizer().await?;
    // A book may contain hundreds of pages. Keep one prepared frame and encoded
    // image in memory instead of retaining the entire rendered book.
    for (folder, members) in groups {
        let width = members.len().to_string().len().max(4);
        let directory = folder
            .as_ref()
            .map_or_else(|| destination.clone(), |folder| destination.join(folder));
        if !matches!(format, ExportFormat::Cbz) {
            tokio::fs::create_dir_all(&directory).await?;
        }
        let mut archive = if matches!(format, ExportFormat::Cbz) {
            Some(zip::ZipWriter::new(tempfile::NamedTempFile::new_in(
                &destination,
            )?))
        } else {
            None
        };
        for (ordinal, page_id) in members.iter().enumerate() {
            anyhow::ensure!(!stop.is_some_and(StopToken::stopped), "export stopped");
            let frame = renderer.render(&snapshot, *page_id).await?;
            let (extension, bytes) = match format {
                ExportFormat::Png | ExportFormat::Cbz => {
                    let rasterizer = Arc::clone(&rasterizer);
                    let bytes = tokio_rayon::spawn(move || -> Result<Vec<u8>> {
                        let image = rasterizer
                            .rasterize(&frame.raster_frame()?, RasterOptions::default())?
                            .image;
                        let mut bytes = Vec::new();
                        PngEncoder::new_with_quality(
                            &mut bytes,
                            CompressionType::Best,
                            FilterType::Adaptive,
                        )
                        .write_image(
                            image.as_raw(),
                            image.width(),
                            image.height(),
                            ExtendedColorType::Rgba8,
                        )?;
                        Ok(bytes)
                    })
                    .await?;
                    ("png", bytes)
                }
                ExportFormat::Psd => (
                    "psd",
                    export_page(
                        Arc::clone(&rasterizer),
                        &snapshot,
                        &frame,
                        &PsdExportOptions::default(),
                    )
                    .await?,
                ),
            };
            let page = snapshot.page(*page_id)?.page()?;
            let filename = format!(
                "{:0width$}_{}.{extension}",
                ordinal + 1,
                page_stem(&page.label),
                width = width,
            );
            let target = directory.join(&filename);
            archive = tokio::task::spawn_blocking(move || -> Result<_> {
                if let Some(archive) = &mut archive {
                    // PNG data is already compressed.
                    let options = zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored);
                    archive.start_file(filename, options)?;
                    archive.write_all(&bytes)?;
                } else {
                    std::fs::write(target, bytes)?;
                }
                Ok(archive)
            })
            .await??;
        }
        if let Some(archive) = archive {
            let filename = match folder {
                Some(folder) => format!("{folder}.cbz"),
                None => format!("{name}.cbz"),
            };
            let target = destination.join(filename);
            tokio::task::spawn_blocking(move || -> Result<()> {
                archive.finish()?.persist(target)?;
                Ok(())
            })
            .await??;
        }
    }
    Ok(())
}

/// Chapter folders in roster order, with every page outside a chapter last. A
/// project without chapters has nothing to separate, so it keeps writing
/// straight into the picked folder.
fn export_groups(chapters: &Chapters, pages: &[EntityId]) -> Vec<(Option<String>, Vec<EntityId>)> {
    if chapters.entries.is_empty() {
        return vec![(None, pages.to_vec())];
    }
    let mut groups = Vec::new();
    let mut assigned = HashSet::new();
    let mut used = HashSet::new();
    for chapter in &chapters.entries {
        let members = pages
            .iter()
            .copied()
            .filter(|page| chapter.pages.contains(page))
            .collect::<Vec<_>>();
        if members.is_empty() {
            continue;
        }
        assigned.extend(members.iter().copied());
        let folder = unique_folder(&mut used, &chapter.title);
        groups.push((Some(folder), members));
    }
    let unassigned = pages
        .iter()
        .copied()
        .filter(|page| !assigned.contains(page))
        .collect::<Vec<_>>();
    if !unassigned.is_empty() {
        groups.push((Some("unassigned".to_owned()), unassigned));
    }
    groups
}

/// The folder is the chapter title exactly as it appears in the rail, with only
/// the characters Windows rejects replaced. Two chapters sharing a title would
/// write into one folder, so the second and later ones get a numeric suffix.
fn unique_folder(used: &mut HashSet<String>, title: &str) -> String {
    let title = sanitize(title);
    let title = if title.is_empty() {
        "chapter".to_owned()
    } else {
        title
    };
    if used.insert(title.clone()) {
        return title;
    }
    for suffix in 2.. {
        let candidate = format!("{title} ({suffix})");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("an unbounded suffix always finds a free name")
}

/// Writes the wiki as `wiki/<slug>.md` inside the picked folder, which is the
/// layout the wiki importer reads back, so a backup restores as-is.
#[tauri::command]
#[specta::specta]
pub(crate) async fn export_wiki(
    window: WebviewWindow<CefRuntime>,
    project: State<'_, CurrentProject>,
) -> std::result::Result<(), Error> {
    let wiki = {
        let project = project.project.lock().await;
        let project = project.as_ref().context("no project is open")?;
        project.snapshot().wiki()?
    };
    if wiki.files.iter().all(|file| file.body.trim().is_empty()) {
        return Err(anyhow::anyhow!("the wiki has no content to export").into());
    }
    let Some(destination) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .pick_folder()
        .await
        .map(|destination| destination.path().to_owned())
    else {
        return Ok(());
    };
    let directory = destination.join("wiki");
    tokio_rayon::spawn(move || -> Result<()> {
        std::fs::create_dir_all(&directory)?;
        for file in &wiki.files {
            if file.body.trim().is_empty() {
                continue;
            }
            std::fs::write(directory.join(format!("{}.md", file.slug)), &file.body)?;
        }
        Ok(())
    })
    .await?;
    Ok(())
}

/// Windows rejects these characters and a trailing dot in a path segment.
fn sanitize(value: &str) -> String {
    value
        .replace(['<', '>', ':', '"', '/', '\\', '|', '?', '*'], "_")
        .trim()
        .trim_end_matches('.')
        .trim()
        .to_owned()
}

fn page_stem(label: &str) -> String {
    let trimmed = label
        .trim()
        .trim_end_matches(|character: char| character == '.' || character.is_whitespace());
    let stem = trimmed.rsplit_once('.').map_or(trimmed, |(stem, _)| stem);
    let stem = sanitize(stem);
    if stem.is_empty() {
        "page".to_owned()
    } else {
        stem
    }
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_thumbnail(
    page: EntityId,
    project: State<'_, CurrentProject>,
) -> std::result::Result<ImageBytes, Error> {
    let snapshot = project
        .project
        .lock()
        .await
        .as_ref()
        .context("no project is open")?
        .snapshot();
    snapshot.page(page)?;
    let blob = snapshot
        .asset(page, &AssetRole::new("source")?)?
        .with_context(|| format!("page {page} has no source image"))?
        .blob;
    let bytes = snapshot.read_blob(blob).await?;
    let bytes = tokio_rayon::spawn(move || -> Result<Vec<u8>> {
        let image = image::load_from_memory(&bytes).context("failed to decode source image")?;
        if image.width() == 0 || image.height() == 0 {
            return Err(anyhow::anyhow!("source image is empty"));
        }
        let image = image.thumbnail(THUMBNAIL_EDGE, THUMBNAIL_EDGE).to_rgba8();
        let encoder = webp::Encoder::from_rgba(image.as_raw(), image.width(), image.height());
        Ok(encoder.encode(80.0).to_vec())
    })
    .await?;
    Ok(ImageBytes(bytes))
}

pub(crate) async fn rendered_preview(
    renderer: &Renderer,
    rasterizer: Arc<Rasterizer>,
    snapshot: &Snapshot,
    page: EntityId,
) -> Result<Vec<u8>> {
    snapshot.page(page)?;
    let frame = renderer.render(snapshot, page).await?;
    let image = rasterize(rasterizer, &frame, RasterOptions::default())
        .await?
        .image;
    tokio_rayon::spawn(move || {
        let image = image::DynamicImage::ImageRgba8(image)
            .resize(1024, 1024, image::imageops::FilterType::Lanczos3)
            .to_rgba8();
        let encoder = webp::Encoder::from_rgba(image.as_raw(), image.width(), image.height());
        Ok::<_, anyhow::Error>(encoder.encode(85.0).to_vec())
    })
    .await
}

async fn rasterize(
    rasterizer: Arc<Rasterizer>,
    frame: &Frame,
    options: RasterOptions,
) -> Result<Raster> {
    let frame = frame.raster_frame()?;
    tokio_rayon::spawn(move || rasterizer.rasterize(&frame, options))
        .await
        .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::{export_groups, page_stem, sanitize};
    use koharu_scene::{Chapter, Chapters, EntityId};

    /// Folder names are the chapter titles themselves, so a chapter titled "11"
    /// exports to `11`. A page outside every chapter still lands in its own
    /// folder, and duplicate titles stay distinct.
    #[test]
    fn chapters_become_one_folder_named_after_the_chapter() {
        let pages = (0..6).map(|_| EntityId::new()).collect::<Vec<_>>();
        let mut eleven = Chapter::new("11");
        eleven.pages = vec![pages[0], pages[1]];
        let mut twelve = Chapter::new("12");
        twelve.pages = vec![pages[2]];
        let mut prologue = Chapter::new("Prologue: A/B");
        prologue.pages = vec![pages[3]];
        let mut twin = Chapter::new("12");
        twin.pages = vec![pages[4]];
        let chapters = Chapters {
            entries: vec![eleven, twelve, prologue, twin],
        };
        let groups = export_groups(&chapters, &pages);
        let summary = groups
            .iter()
            .map(|(folder, members)| (folder.clone(), members.len()))
            .collect::<Vec<_>>();
        assert_eq!(
            summary,
            vec![
                (Some("11".to_owned()), 2),
                (Some("12".to_owned()), 1),
                (Some("Prologue_ A_B".to_owned()), 1),
                (Some("12 (2)".to_owned()), 1),
                (Some("unassigned".to_owned()), 1),
            ]
        );
        assert_eq!(groups[4].1, vec![pages[5]]);
    }

    /// A project with no chapters keeps the flat layout it always had.
    #[test]
    fn a_project_without_chapters_exports_flat() {
        let pages = (0..3).map(|_| EntityId::new()).collect::<Vec<_>>();
        assert_eq!(
            export_groups(&Chapters::default(), &pages),
            vec![(None, pages)]
        );
    }

    #[test]
    fn titles_and_labels_stay_valid_path_segments() {
        assert_eq!(sanitize("A/B: C?"), "A_B_ C_");
        assert_eq!(sanitize("Chapter 01."), "Chapter 01");
        assert_eq!(page_stem("001.jpg"), "001");
        assert_eq!(page_stem("  ..  "), "page");
    }
}
