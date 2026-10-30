//! Independent page checkpoints let OCR and cleanup commit concurrently.
use crate::{BlobId, Component, Origin, Result, ValidationContext};
use revision::revisioned;
use serde::{Deserialize, Serialize};
use specta::Type;

macro_rules! checkpoint {
    ($name:ident, $kind:literal) => {
        #[revisioned(revision = 1)]
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
        pub struct $name {
            pub origin: Origin,
            pub input: BlobId,
        }
        impl Component for $name {
            const KIND: &'static str = $kind;
            fn validate(&self, _: &ValidationContext<'_>) -> Result<()> {
                self.origin.validate()
            }
            fn origin(&self) -> Option<&Origin> {
                Some(&self.origin)
            }
            fn set_origin(&mut self, origin: Origin) -> bool {
                self.origin = origin;
                true
            }
        }
    };
}
checkpoint!(DetectionCheckpoint, "dev.koharu.checkpoint.detection");
checkpoint!(OcrCheckpoint, "dev.koharu.checkpoint.ocr");
checkpoint!(TranslationCheckpoint, "dev.koharu.checkpoint.translation");
checkpoint!(InpaintingCheckpoint, "dev.koharu.checkpoint.inpainting");
