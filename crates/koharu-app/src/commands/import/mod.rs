use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, Result, bail};
use image::{ImageFormat, ImageReader};
use rayon::prelude::*;
use strum::{EnumIter, EnumMessage, EnumString};

mod pdf;
mod rar;
mod zip;

#[derive(Clone, Copy, EnumIter, EnumMessage, EnumString)]
#[strum(ascii_case_insensitive)]
pub(super) enum Format {
    #[strum(
        serialize = "png",
        serialize = "jpg",
        serialize = "jpeg",
        serialize = "webp"
    )]
    Raster,
    #[strum(serialize = "cbz", serialize = "zip")]
    Zip,
    #[strum(serialize = "rar")]
    Rar,
    #[strum(serialize = "pdf")]
    Pdf,
}

#[derive(Debug)]
pub(super) struct EncodedPage {
    pub(super) name: String,
    pub(super) bytes: Vec<u8>,
}

pub(crate) struct ImportedPage {
    pub(crate) name: String,
    pub(crate) bytes: Arc<[u8]>,
    pub(crate) format: ImageFormat,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

pub(crate) struct ImportedChapter {
    pub(crate) title: String,
    pub(crate) pages: Vec<ImportedPage>,
}

fn decode(path: &Path, source: EncodedPage) -> Result<ImportedPage> {
    let EncodedPage { name, bytes } = source;
    let format = image::guess_format(&bytes).with_context(|| {
        format!(
            "failed to identify imported image {} ({name})",
            path.display()
        )
    })?;
    let (width, height) = ImageReader::with_format(Cursor::new(bytes.as_slice()), format)
        .into_dimensions()
        .with_context(|| {
            format!(
                "failed to read dimensions of imported image {} ({name})",
                path.display()
            )
        })?;
    Ok(ImportedPage {
        name,
        bytes: Arc::<[u8]>::from(bytes),
        format,
        width,
        height,
    })
}

pub(super) fn import(mut paths: Vec<PathBuf>) -> Result<Vec<ImportedPage>> {
    alphanumeric_sort::sort_slice_by_os_str_key(&mut paths, |path| {
        path.file_name().unwrap_or_else(|| path.as_os_str())
    });
    import_presorted(paths)
}

/// Splits a root folder into chapters: supported files directly in the root
/// become the first chapter, then each immediate subfolder is imported in
/// natural order. Files nested deeper belong to their top-level folder.
pub(super) fn import_folder(root: &Path) -> Result<Vec<ImportedChapter>> {
    let mut folders = Vec::new();
    let mut root_files = Vec::new();
    for entry in fs::read_dir(root).with_context(|| format!("failed to list {}", root.display()))? {
        let entry = entry.with_context(|| format!("failed to list {}", root.display()))?;
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            folders.push(path);
        } else if kind.is_file() && is_page(&path) {
            root_files.push(path);
        }
    }
    alphanumeric_sort::sort_slice_by_os_str_key(&mut folders, |path| {
        path.file_name().unwrap_or_else(|| path.as_os_str())
    });

    let mut chapters = Vec::new();
    if !root_files.is_empty() {
        let title = folder_title(root);
        chapters.push(ImportedChapter {
            title,
            pages: import_group(root, root_files)?,
        });
    }
    for folder in folders {
        let files = collect_pages(&folder);
        if files.is_empty() {
            continue;
        }
        let title = folder_title(&folder);
        chapters.push(ImportedChapter {
            title,
            pages: import_group(&folder, files)?,
        });
    }
    if chapters.is_empty() {
        bail!("no supported images were found in the selection");
    }
    Ok(chapters)
}

/// Every supported page under `root`, in natural order, as one flat group. Used
/// when the caller picked a chapter to import into instead of splitting by folder.
pub(super) fn import_flat(root: &Path) -> Result<Vec<ImportedPage>> {
    let files = collect_pages(root);
    if files.is_empty() {
        bail!("no supported images were found in the selection");
    }
    import_group(root, files)
}

fn collect_pages(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| match entry {
            Ok(entry) if entry.file_type().is_file() && is_page(entry.path()) => {
                Some(entry.into_path())
            }
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(%error, "could not inspect an import directory entry");
                None
            }
        })
    {
        files.push(entry);
    }
    files
}

fn import_group(root: &Path, mut paths: Vec<PathBuf>) -> Result<Vec<ImportedPage>> {
    alphanumeric_sort::sort_slice_by_os_str_key(&mut paths, |path| {
        path.strip_prefix(root).unwrap_or(path)
    });
    import_presorted(paths)
}

fn folder_title(path: &Path) -> String {
    path.file_name()
        .filter(|name| !name.is_empty())
        .map_or_else(
            || "Chapter".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        )
}

fn is_page(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.parse::<Format>().is_ok())
}

fn import_presorted(paths: Vec<PathBuf>) -> Result<Vec<ImportedPage>> {
    let mut groups = paths
        .into_par_iter()
        .map(|path| -> Result<Vec<ImportedPage>> {
            let extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .and_then(|extension| extension.parse::<Format>().ok());
            let encoded = match extension {
                Some(Format::Raster) => vec![EncodedPage {
                    name: path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "page".to_owned()),
                    bytes: fs::read(&path)
                        .with_context(|| format!("failed to read {}", path.display()))?,
                }],
                Some(Format::Zip) => zip::extract(&path)?,
                Some(Format::Rar) => rar::extract(&path)?,
                Some(Format::Pdf) => pdf::render(&path)?,
                None => bail!("unsupported page import path {}", path.display()),
            };
            encoded
                .into_iter()
                .map(|source| decode(&path, source))
                .collect()
        })
        .collect::<Result<Vec<_>>>()?;
    let page_count = groups.iter().map(Vec::len).sum();
    let mut pages = Vec::with_capacity(page_count);
    for group in &mut groups {
        pages.append(group);
    }
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_paths_are_naturally_sorted() {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "koharu-import-order-{}-{timestamp}",
            std::process::id()
        ));
        fs::create_dir(&directory).expect("create fixture directory");
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]));
        let mut encoded = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut encoded, ImageFormat::Png)
            .expect("encode fixture");
        let paths = ["page10.PNG", "page2.png", "page1.png"].map(|name| directory.join(name));
        for path in &paths {
            fs::write(path, encoded.get_ref()).expect("write fixture");
        }

        let pages = import(paths.into()).expect("import fixtures");
        fs::remove_dir_all(&directory).expect("remove fixture directory");
        assert_eq!(
            pages
                .iter()
                .map(|page| page.name.as_str())
                .collect::<Vec<_>>(),
            ["page1.png", "page2.png", "page10.PNG"]
        );
    }

    #[test]
    fn folder_import_groups_immediate_folders_as_ordered_chapters() {
        let directory = std::env::temp_dir().join(format!(
            "koharu-chapter-import-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        let chapter_10 = directory.join("Chapter 10");
        let chapter_2 = directory.join("Chapter 2");
        fs::create_dir_all(&chapter_10).expect("create chapter");
        fs::create_dir_all(&chapter_2).expect("create chapter");
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]));
        let mut encoded = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut encoded, ImageFormat::Png)
            .expect("encode fixture");
        for path in [
            directory.join("cover.png"),
            chapter_10.join("002.png"),
            chapter_10.join("001.png"),
            chapter_2.join("001.png"),
        ] {
            fs::write(path, encoded.get_ref()).expect("write fixture");
        }

        let chapters = import_folder(&directory).expect("import folder");
        fs::remove_dir_all(&directory).expect("remove fixture directory");
        assert_eq!(
            chapters
                .iter()
                .map(|chapter| chapter.title.as_str())
                .collect::<Vec<_>>(),
            [
                directory.file_name().unwrap().to_string_lossy().as_ref(),
                "Chapter 2",
                "Chapter 10"
            ]
        );
        assert_eq!(
            chapters[1]
                .pages
                .iter()
                .map(|page| page.name.as_str())
                .collect::<Vec<_>>(),
            ["001.png"]
        );
        assert_eq!(
            chapters[2]
                .pages
                .iter()
                .map(|page| page.name.as_str())
                .collect::<Vec<_>>(),
            ["001.png", "002.png"]
        );
    }
}
