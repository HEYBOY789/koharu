//! One-way conversion from the directory-based fork project into a new scene.

use std::{collections::BTreeSet, path::Path};

use anyhow::{Context, Result, ensure};
use koharu_scene as scene;
use legacy_scene as legacy;

mod layers;

#[derive(Debug)]
pub struct Report {
    pub pages: usize,
    pub chapters: usize,
    pub text_layers: usize,
}

/// Publishes a completed file only after the new scene has reopened it.
/// Fails if a project with that name already exists.
pub async fn convert(source: &Path, destination: &Path) -> Result<Report> {
    ensure!(
        source.is_dir()
            && source
                .extension()
                .is_some_and(|value| value.eq_ignore_ascii_case("khrproj")),
        "select a .khrproj directory"
    );
    ensure!(
        destination
            .extension()
            .is_some_and(|value| value.eq_ignore_ascii_case(scene::EXTENSION)),
        "destination must have the .khr extension"
    );
    ensure!(
        !destination.exists(),
        "a project already exists at {}",
        destination.display()
    );

    let legacy = legacy::Session::open(source)
        .await
        .with_context(|| format!("open {}", source.display()))?;
    let snapshot = legacy.snapshot();
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let staging = tempfile::tempdir_in(parent)?;
    let path = staging.path().join("project.khr");
    let mut session = scene::Session::create(&path)?;
    let source_role = legacy::AssetRole::new("source")?;
    let mut metadata = scene::ProjectMetadata {
        chapters: serde_json::from_value(serde_json::to_value(snapshot.chapters()?)?)?,
        wiki: serde_json::from_value(serde_json::to_value(snapshot.wiki()?)?)?,
        publication: serde_json::from_value(serde_json::to_value(
            snapshot.project_component::<legacy::PublicationMetadata>()?,
        )?)?,
        source: serde_json::from_value(serde_json::to_value(
            snapshot.project_component::<legacy::ImportedSource>()?,
        )?)?,
        ..Default::default()
    };
    let mut text_layers = 0;
    let mut translated_pages = BTreeSet::new();

    for (index, page) in snapshot.pages().enumerate() {
        let id = scene::PageId(page.id().as_uuid());
        let data = page.page()?;
        let (width, height) = (data.width.round() as u32, data.height.round() as u32);
        let mut ops = Vec::new();
        let background = match snapshot.asset(page.id(), &source_role)? {
            Some(asset) => Some(file(&snapshot, asset, &mut ops).await?),
            None => None,
        };
        ops.push(scene::Op::AddPage {
            id: Some(id.into()),
            index,
            size: background
                .is_none()
                .then_some(scene::Size { width, height }),
            background,
        });
        let mut builder =
            layers::PageLayers::new(&snapshot, &source_role, id, width, height, &mut ops);
        builder.children(page.id(), None).await?;
        text_layers += builder.text_layers;
        session
            .apply(scene::Edit {
                label: "Convert page".into(),
                ops,
            })
            .with_context(|| format!("convert page {} ({})", data.label, id))?;
        metadata.page_labels.insert(id, data.label);
        if snapshot
            .component::<legacy::TranslationCheckpoint>(page.id())?
            .is_some()
        {
            translated_pages.insert(id);
        }
    }
    for chapter in &mut metadata.chapters.entries {
        chapter.translated = !chapter.pages.is_empty()
            && chapter
                .pages
                .iter()
                .all(|page| translated_pages.contains(page));
    }
    let mut ops = Vec::new();
    let cover_role = legacy::AssetRole::new("cover")?;
    for page in snapshot.pages() {
        if let Some(asset) = snapshot.asset(page.id(), &cover_role)? {
            metadata.cover = Some(file(&snapshot, asset, &mut ops).await?);
            break;
        }
    }
    let report = Report {
        pages: snapshot.pages().len(),
        chapters: metadata.chapters.entries.len(),
        text_layers,
    };
    ops.push(scene::Op::SetProjectMetadata {
        metadata: Box::new(metadata),
    });
    session.apply(scene::Edit {
        label: "Convert chapter and project data".into(),
        ops,
    })?;
    drop(session);
    drop(scene::Session::open(&path)?);
    // A hard link publishes without replacing a file created by another
    // process during conversion. Staging is on the same volume.
    std::fs::hard_link(&path, destination)
        .with_context(|| format!("publish {}", destination.display()))?;
    Ok(report)
}

async fn file(
    snapshot: &legacy::Snapshot,
    asset: legacy::Asset,
    ops: &mut Vec<scene::Op>,
) -> Result<scene::Hash> {
    let bytes = snapshot.read_blob(asset.blob).await?.to_vec();
    let hash = scene::Hash::of(&bytes);
    ops.push(scene::Op::AddFile { bytes });
    Ok(hash)
}
