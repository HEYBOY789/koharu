mod detection;
mod inpainting;
mod ocr;
mod translation;

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use anyhow::Result;
use async_trait::async_trait;
use koharu_scene::{Edit, EntityId, Generation, Patch, ProducerId, Snapshot};

pub use detection::KoharuLayoutRFDetrSeg2XLConfig;
pub use inpainting::{Flux2KleinConfig, RoremMixedConfig};

use crate::{Bounds, ImageCache, InpaintingMask, PipelineConfig, Stage};

#[derive(Clone)]
pub(crate) struct StageInput {
    pub(crate) scene: koharu_scene::Snapshot,
    pub(crate) page: EntityId,
    pub(crate) entities: Option<Arc<BTreeSet<EntityId>>>,
    pub(crate) region: Option<Bounds>,
    images: Arc<ImageCache>,
    pub(crate) inpainting_mask: Option<InpaintingMask>,
    pub(crate) resume: bool,
    // One immutable page job owns these uncommitted results across attempts.
    translated: Arc<tokio::sync::Mutex<BTreeMap<EntityId, String>>>,
}

impl StageInput {
    pub(crate) fn new(
        scene: Snapshot,
        page: EntityId,
        entities: Option<Arc<BTreeSet<EntityId>>>,
        region: Option<Bounds>,
        images: Arc<ImageCache>,
        inpainting_mask: Option<InpaintingMask>,
    ) -> Self {
        Self {
            translated: Arc::default(),
            resume: false,
            scene,
            page,
            entities,
            region,
            images,
            inpainting_mask,
        }
    }

    pub(crate) fn page(&self) -> EntityId {
        self.page
    }

    pub(crate) fn with_resume(mut self, resume: bool) -> Self {
        self.resume = resume;
        self
    }

    fn contains_entity(&self, entity: EntityId) -> Result<bool> {
        crate::scope::contains_entity(
            &self.scene,
            self.page,
            self.entities.as_deref(),
            self.region,
            entity,
        )
    }
}

#[async_trait]
pub(crate) trait StageProcessor: Send + Sync {
    fn model(&self) -> &'static str;
    fn skip(&self, _input: &StageInput) -> Result<bool> {
        Ok(false)
    }
    fn unload(&self) -> bool;
    async fn load(&self) -> Result<()>;
    async fn process(&self, input: StageInput) -> Result<Patch>;
}

pub(crate) struct Stages {
    language: String,
    detection: Box<dyn StageProcessor>,
    ocr: Box<dyn StageProcessor>,
    translation: Box<dyn StageProcessor>,
    inpainting: Box<dyn StageProcessor>,
}

impl Stages {
    #[cfg(test)]
    pub(crate) fn replace_translation(&mut self, processor: Box<dyn StageProcessor>) {
        self.translation = processor;
    }
    pub(crate) fn new(
        config: &PipelineConfig,
        translator: koharu_translator::Translator,
        device: &koharu_ml::Device,
    ) -> Result<Self> {
        Ok(Self {
            language: config.translation.target_language.tag().to_owned(),
            detection: Box::new(detection::Processor::new(
                config.detection()?,
                device.clone(),
            )),
            ocr: Box::new(ocr::Processor::new(config.ocr.clone(), device.clone())),
            translation: Box::new(translation::Processor::new(
                config.translation.clone(),
                translator,
            )),
            inpainting: Box::new(inpainting::Processor::new(
                config.inpainting()?,
                device.clone(),
            )?),
        })
    }

    fn processor(&self, stage: Stage) -> &dyn StageProcessor {
        match stage {
            Stage::Detection => self.detection.as_ref(),
            Stage::Ocr => self.ocr.as_ref(),
            Stage::Translation => self.translation.as_ref(),
            Stage::Inpainting => self.inpainting.as_ref(),
        }
    }

    pub(crate) fn model(&self, stage: Stage) -> &'static str {
        self.processor(stage).model()
    }

    pub(crate) fn skip(&self, stage: Stage, input: &StageInput) -> Result<bool> {
        if input.resume
            && input.entities.is_none()
            && input.region.is_none()
            && input.inpainting_mask.is_none()
        {
            return crate::resume::complete(&input.scene, input.page, stage, &self.language);
        }
        self.processor(stage).skip(input)
    }

    pub(crate) fn pending_pages(
        &self,
        scene: &Snapshot,
        pages: &[EntityId],
    ) -> Result<Vec<EntityId>> {
        let mut pending = Vec::new();
        for page in pages {
            let mut complete = true;
            for stage in Stage::ALL {
                if !crate::resume::complete(scene, *page, stage, &self.language)? {
                    complete = false;
                    break;
                }
            }
            if !complete {
                pending.push(*page);
            }
        }
        Ok(pending)
    }

    pub(crate) async fn load(&self, stage: Stage) -> Result<()> {
        self.processor(stage).load().await
    }

    pub(crate) async fn process(&self, stage: Stage, input: StageInput) -> Result<Patch> {
        self.processor(stage).process(input).await
    }

    pub(crate) fn unload(&self, stage: Stage) -> bool {
        self.processor(stage).unload()
    }
}

fn generation(producer: &str, model: &str) -> Result<Generation> {
    let mut generation = Generation::new(ProducerId::new(producer)?);
    generation.model = Some(model.to_owned());
    Ok(generation)
}

fn finish(edit: Edit) -> Result<Patch> {
    edit.finish().map_err(Into::into)
}

fn checkpoint(input: &StageInput, stage: Stage, language: &str, mut edit: Edit) -> Result<Patch> {
    crate::resume::record(input, stage, language, &mut edit)?;
    finish(edit)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(bytes: &'static [u8]) -> koharu_scene::AssetInput {
        koharu_scene::AssetInput::new(
            bytes,
            "image/png",
            koharu_scene::AssetMetadata {
                width: Some(1),
                height: Some(1),
                attributes: std::collections::BTreeMap::new(),
            },
        )
    }

    #[tokio::test]
    async fn registry_contains_every_stage() {
        let translator = koharu_translator::Translator::from_config(
            koharu_ml::Device::cpu(),
            koharu_config::Config::memory(koharu_translator::ProvidersConfig::default()),
        )
        .unwrap();
        let stages = Stages::new(
            &PipelineConfig::default(),
            translator,
            &koharu_ml::Device::cpu(),
        )
        .unwrap();

        assert_eq!(
            Stage::ALL.map(|stage| stages.model(stage)),
            [
                "koharu-layout-rfdetr-seg-2xl",
                "paddleocr-vl-1.6",
                "local",
                "lama",
            ]
        );
    }

    #[tokio::test]
    async fn translation_and_inpainting_compose_without_weakening_text_guards() {
        let mut session = koharu_scene::Session::memory().await.unwrap();
        let mut setup = session.snapshot().edit();
        let page = setup
            .add_page(
                koharu_scene::PageDraft::new("page", 1.0, 1.0),
                koharu_scene::At::End,
            )
            .unwrap();
        let text = setup.add_text_content(page, koharu_scene::At::End).unwrap();
        setup
            .set(
                text,
                &koharu_scene::SourceText {
                    text: koharu_scene::Authored::user("before".to_owned()),
                    language: None,
                },
            )
            .unwrap();
        setup
            .set_asset(
                page,
                &koharu_scene::AssetRole::new("source").unwrap(),
                asset(b"source"),
            )
            .unwrap();
        session.commit(setup.finish().unwrap()).await.unwrap();
        let base = session.snapshot();

        let mut text_edit = base.edit();
        text_edit.observe::<koharu_scene::SourceText>(text).unwrap();
        text_edit
            .observe::<koharu_scene::Translation>(text)
            .unwrap();
        text_edit
            .set(
                text,
                &koharu_scene::Translation {
                    text: koharu_scene::Authored::user("after".to_owned()),
                    language: None,
                },
            )
            .unwrap();
        let text_patch = text_edit.finish().unwrap();

        let mut image_edit = base.edit();
        image_edit.observe_assets(page).unwrap();
        let cleanup = image_edit
            .add_entity(page, koharu_scene::At::Start)
            .unwrap();
        image_edit
            .set(
                cleanup,
                &koharu_scene::RasterLayer {
                    origin: koharu_scene::Origin::User,
                    name: "Cleanup".to_owned(),
                    kind: koharu_scene::RasterLayerKind::Cleanup,
                },
            )
            .unwrap();
        image_edit
            .set_asset(
                cleanup,
                &koharu_scene::AssetRole::new("source").unwrap(),
                asset(b"clean"),
            )
            .unwrap();
        let image_patch = image_edit.finish().unwrap();

        let image_first = base.preview([&image_patch]).unwrap();
        assert!(text_patch.rebase_on(&image_first).is_ok());
        let text_first = base.preview([&text_patch]).unwrap();
        assert!(image_patch.rebase_on(&text_first).is_ok());

        let changed_source = base
            .patch(|edit| {
                edit.set(
                    text,
                    &koharu_scene::SourceText {
                        text: koharu_scene::Authored::user("changed".to_owned()),
                        language: None,
                    },
                )
            })
            .unwrap();
        let changed_source = base.preview([&changed_source]).unwrap();
        assert!(text_patch.rebase_on(&changed_source).is_err());
    }
}
