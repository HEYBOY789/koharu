use crate::{Stage, stages::StageInput};
use anyhow::Result;
use koharu_scene::{
    AssetRole, BlobId, BubbleRegion, DetectionAnalysis, DetectionCheckpoint, Edit, EntityId,
    Geometry, InpaintingCheckpoint, OcrCheckpoint, Origin, RasterLayer, RasterLayerKind, Region,
    RegionSpec, Snapshot, TextRegion, Translation, TranslationCheckpoint,
};

fn append(bytes: &mut Vec<u8>, value: &[u8]) {
    bytes.extend_from_slice(&(value.len() as u64).to_le_bytes());
    bytes.extend_from_slice(value);
}

#[cfg(test)]
pub(crate) async fn text_fixture(
    count: usize,
) -> (koharu_scene::Session, Vec<(EntityId, EntityId)>) {
    use koharu_scene::{At, Authored, PageDraft, SourceText, TextLayout, TextLayoutKind};
    let mut session = koharu_scene::Session::memory().await.unwrap();
    let mut edit = session.snapshot().edit();
    let mut ids = Vec::new();
    for index in 0..count {
        let page = edit
            .add_page(
                PageDraft::new(format!("page {index}"), 100.0, 100.0),
                At::End,
            )
            .unwrap();
        let content = edit.add_text_content(page, At::End).unwrap();
        edit.set(
            content,
            &SourceText {
                text: Authored::user(format!("source {index}")),
                language: None,
            },
        )
        .unwrap();
        edit.add_text_layer(
            page,
            At::End,
            content,
            &TextLayout {
                origin: Origin::User,
                kind: TextLayoutKind::Paragraph,
                angle_degrees: None,
            },
        )
        .unwrap();
        ids.push((page, content));
    }
    session.commit(edit.finish().unwrap()).await.unwrap();
    (session, ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use koharu_scene::{At, Authored, Generation, LanguageTag, PageDraft, ProducerId, SourceText};
    use std::sync::Arc;
    fn input(scene: Snapshot, page: EntityId) -> StageInput {
        StageInput::new(scene, page, None, None, Arc::default(), None).with_resume(true)
    }

    #[tokio::test]
    async fn resume_checks_language_empty_text_and_source_changes() {
        let (mut session, ids) = text_fixture(1).await;
        let (page, content) = ids[0];
        let generation =
            Generation::new(ProducerId::new("dev.koharu.pipeline.translation").unwrap());
        for (language, text, done) in [
            ("vi", "done", true),
            ("en", "done", false),
            ("vi", "", false),
        ] {
            let mut edit = session.snapshot().edit_as(generation.clone());
            edit.set(
                content,
                &Translation {
                    text: Authored::generated(text.to_owned(), generation.clone()),
                    language: Some(LanguageTag::new(language).unwrap()),
                },
            )
            .unwrap();
            session.commit(edit.finish().unwrap()).await.unwrap();
            assert_eq!(
                complete(&session.snapshot(), page, Stage::Translation, "vi").unwrap(),
                done
            );
        }
        let old = input(session.snapshot(), page);
        let mut edit = session.snapshot().edit_as(generation.clone());
        edit.set(
            content,
            &Translation {
                text: Authored::generated("done".into(), generation.clone()),
                language: Some(LanguageTag::new("vi").unwrap()),
            },
        )
        .unwrap();
        record(&old, Stage::Translation, "vi", &mut edit).unwrap();
        session.commit(edit.finish().unwrap()).await.unwrap();
        assert!(complete(&session.snapshot(), page, Stage::Translation, "vi").unwrap());
        let patch = session
            .snapshot()
            .patch(|edit| {
                edit.set(
                    content,
                    &SourceText {
                        text: Authored::user("edited source".into()),
                        language: None,
                    },
                )
            })
            .unwrap();
        session.commit(patch).await.unwrap();
        assert!(!complete(&session.snapshot(), page, Stage::Translation, "vi").unwrap());
    }

    #[tokio::test]
    async fn blank_pages_need_a_committed_detection_proof() {
        let mut session = koharu_scene::Session::memory().await.unwrap();
        let mut edit = session.snapshot().edit();
        let page = edit
            .add_page(PageDraft::new("blank", 1.0, 1.0), At::End)
            .unwrap();
        session.commit(edit.finish().unwrap()).await.unwrap();
        assert!(!complete(&session.snapshot(), page, Stage::Translation, "vi").unwrap());
        let stage = input(session.snapshot(), page);
        let mut edit = session.snapshot().edit_as(Generation::new(
            ProducerId::new("dev.koharu.pipeline.detection").unwrap(),
        ));
        record(&stage, Stage::Detection, "", &mut edit).unwrap();
        session.commit(edit.finish().unwrap()).await.unwrap();
        for step in Stage::ALL {
            assert!(complete(&session.snapshot(), page, step, "vi").unwrap());
        }
    }
}

pub(crate) fn fingerprint(
    scene: &Snapshot,
    page: EntityId,
    stage: Stage,
    language: &str,
) -> Result<BlobId> {
    let mut bytes = Vec::new();
    append(&mut bytes, stage.to_string().as_bytes());
    if let Some(source) = scene.asset(page, &AssetRole::new("source")?)? {
        append(&mut bytes, source.blob.as_bytes());
    }
    let size = scene.page(page)?.page()?;
    append(&mut bytes, &size.width.to_le_bytes());
    append(&mut bytes, &size.height.to_le_bytes());
    if stage != Stage::Detection {
        for entity in scene.descendants(page)? {
            if let Some(region) = entity.component::<Region>()? {
                append(&mut bytes, entity.id().to_string().as_bytes());
                append(&mut bytes, region.kind.as_str().as_bytes());
                if let Some(geometry) = entity.component::<Geometry>()? {
                    for point in geometry.points {
                        append(&mut bytes, &point.x.to_le_bytes());
                        append(&mut bytes, &point.y.to_le_bytes());
                    }
                }
            }
        }
    }
    if stage == Stage::Translation {
        append(&mut bytes, language.as_bytes());
        if let Some(group) = scene.page(page)?.text_group()? {
            for layer in group.text_layers()? {
                let content = layer.content()?;
                append(&mut bytes, content.id().to_string().as_bytes());
                if let Some(source) = content.source()? {
                    append(&mut bytes, source.text.value.as_bytes());
                }
            }
        }
    }
    Ok(BlobId::for_bytes(&bytes))
}

fn stored(scene: &Snapshot, page: EntityId, stage: Stage) -> Result<Option<BlobId>> {
    Ok(match stage {
        Stage::Detection => scene
            .component::<DetectionCheckpoint>(page)?
            .map(|value| value.input),
        Stage::Ocr => scene
            .component::<OcrCheckpoint>(page)?
            .map(|value| value.input),
        Stage::Translation => scene
            .component::<TranslationCheckpoint>(page)?
            .map(|value| value.input),
        Stage::Inpainting => scene
            .component::<InpaintingCheckpoint>(page)?
            .map(|value| value.input),
    })
}

pub(crate) fn changed(
    scene: &Snapshot,
    page: EntityId,
    stage: Stage,
    language: &str,
) -> Result<bool> {
    Ok(match stored(scene, page, stage)? {
        Some(previous) => previous != fingerprint(scene, page, stage, language)?,
        None => false,
    })
}

pub(crate) fn translated(value: &Translation, language: &str) -> bool {
    matches!(value.text.origin, Origin::User)
        || (!value.text.value.trim().is_empty()
            && value
                .language
                .as_ref()
                .is_some_and(|tag| tag.as_str() == language))
}

pub(crate) fn complete(
    scene: &Snapshot,
    page: EntityId,
    stage: Stage,
    language: &str,
) -> Result<bool> {
    let previous = stored(scene, page, stage)?;
    if let Some(previous) = previous {
        if previous != fingerprint(scene, page, stage, language)? {
            return Ok(false);
        }
        if matches!(stage, Stage::Detection | Stage::Inpainting) {
            return Ok(true);
        }
    }
    let group = scene.page(page)?.text_group()?;
    let layers = group
        .map(|group| group.text_layers().map(|layers| layers.collect::<Vec<_>>()))
        .transpose()?
        .unwrap_or_default();
    match stage {
        Stage::Detection => {
            if !layers.is_empty() {
                return Ok(true);
            }
            for entity in scene.descendants(page)? {
                if entity.component::<DetectionAnalysis>()?.is_some()
                    || entity
                        .component::<Region>()?
                        .is_some_and(|region| region.kind == TextRegion::kind())
                {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        Stage::Ocr => {
            if !complete(scene, page, Stage::Detection, language)? {
                return Ok(false);
            }
            for layer in layers {
                if layer.content()?.source()?.is_none() {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Stage::Translation => {
            if !complete(scene, page, Stage::Ocr, language)? {
                return Ok(false);
            }
            for layer in layers {
                let content = layer.content()?;
                let Some(source) = content.source()? else {
                    return Ok(false);
                };
                if source.text.value.trim().is_empty() {
                    continue;
                }
                if !scene
                    .component::<Translation>(content.id())?
                    .is_some_and(|value| translated(&value, language))
                {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        Stage::Inpainting => {
            for entity in scene.children(page)? {
                if scene
                    .component::<RasterLayer>(entity)?
                    .is_some_and(|layer| layer.kind == RasterLayerKind::Cleanup)
                    && scene.asset(entity, &AssetRole::new("source")?)?.is_some()
                {
                    return Ok(true);
                }
            }
            if !complete(scene, page, Stage::Detection, language)? {
                return Ok(false);
            }
            for entity in scene.descendants(page)? {
                if entity.component::<Region>()?.is_some_and(|region| {
                    region.kind == TextRegion::kind() || region.kind == BubbleRegion::kind()
                }) {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }
}

pub(crate) fn record(
    input: &StageInput,
    stage: Stage,
    language: &str,
    edit: &mut Edit,
) -> Result<()> {
    if input.entities.is_some() || input.region.is_some() || input.inpainting_mask.is_some() {
        return Ok(());
    }
    let stamp = fingerprint(&input.scene, input.page, stage, language)?;
    edit.observe_assets(input.page)?;
    match stage {
        Stage::Detection => edit.set(
            input.page,
            &DetectionCheckpoint {
                origin: Origin::User,
                input: stamp,
            },
        )?,
        Stage::Ocr => edit.set(
            input.page,
            &OcrCheckpoint {
                origin: Origin::User,
                input: stamp,
            },
        )?,
        Stage::Translation => edit.set(
            input.page,
            &TranslationCheckpoint {
                origin: Origin::User,
                input: stamp,
            },
        )?,
        Stage::Inpainting => edit.set(
            input.page,
            &InpaintingCheckpoint {
                origin: Origin::User,
                input: stamp,
            },
        )?,
    }
    Ok(())
}
