use anyhow::Result;
use async_trait::async_trait;
use koharu_scene::{Authored, LanguageTag, Origin, SourceText, Translation};
use koharu_translator::{TranslationRequest, Translator};

use crate::TranslationConfig;

use super::{StageInput, StageProcessor, finish, generation};

const PRODUCER: &str = "dev.koharu.pipeline.translation";

pub(super) struct Processor {
    config: TranslationConfig,
    translator: Translator,
}

impl Processor {
    pub(super) fn new(config: TranslationConfig, translator: Translator) -> Self {
        Self { config, translator }
    }
}

#[async_trait]
impl StageProcessor for Processor {
    fn model(&self) -> &'static str {
        Translator::model(&self.config.model)
    }

    fn unload(&self) -> bool {
        self.translator.unload()
    }

    async fn load(&self) -> Result<()> {
        self.translator.load_model(&self.config.model).await
    }

    async fn process(&self, input: StageInput) -> Result<koharu_scene::Patch> {
        let mut targets = Vec::new();
        if let Some(group) = input.scene.page(input.page)?.text_group()? {
            for layer in group.text_layers()? {
                if !input.contains_entity(layer.id())? {
                    continue;
                }
                let content = layer.content()?;
                let Some(source) = content.source()? else {
                    continue;
                };
                if !source.text.value.trim().is_empty() {
                    targets.push((content.id(), source.text.value));
                }
            }
        }
        let mut request = TranslationRequest::new(
            targets.iter().map(|(_, source)| source.clone()),
            self.config.target_language,
        );
        let chapter = input.scene.chapter_for(input.page)?;
        let chapter_setting = chapter
            .as_ref()
            .and_then(|chapter| chapter.setting.as_deref())
            .map(str::trim)
            .filter(|setting| !setting.is_empty());
        let instructions =
            merged_instructions(self.config.instructions.as_deref(), chapter_setting);
        if let Some(instructions) = instructions.as_deref() {
            request = request.with_instructions(instructions);
        }
        if Translator::supports_vision(&self.config.model, &self.config.generation)
            && let Some(image) = input.images.get(&input.scene, input.page, "source").await?
        {
            request = request.with_image(image);
        }
        let (provider, translated) = self
            .translator
            .translate(&self.config.model, self.config.generation, request)
            .await?;
        let language = LanguageTag::new(self.config.target_language.tag())?;
        let generated = generation(PRODUCER, provider)?;
        let mut edit = input.scene.edit_as(generated.clone());
        for (entity, _) in &targets {
            edit.observe::<SourceText>(*entity)?;
            edit.observe::<Translation>(*entity)?;
        }
        for ((entity, source), text) in targets.into_iter().zip(translated) {
            if input
                .scene
                .component::<Translation>(entity)?
                .is_some_and(|value| matches!(value.text.origin, Origin::User))
            {
                continue;
            }
            let text = if source.trim() == "\u{2026}" {
                "\u{2026}".to_owned()
            } else {
                text
            };
            edit.set(
                entity,
                &Translation {
                    text: Authored::generated(text, generated.clone()),
                    language: Some(language.clone()),
                },
            )?;
        }
        finish(edit)
    }
}

fn merged_instructions(global: Option<&str>, chapter: Option<&str>) -> Option<String> {
    match (global, chapter) {
        (Some(global), Some(chapter)) => {
            Some(format!("{global}\n\n## Chapter setting\n\n{chapter}"))
        }
        (Some(instructions), None) | (None, Some(instructions)) => Some(instructions.to_owned()),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::merged_instructions;

    #[test]
    fn chapter_setting_is_appended_to_global_instructions() {
        assert_eq!(
            merged_instructions(Some("global"), Some("chapter")).as_deref(),
            Some("global\n\n## Chapter setting\n\nchapter")
        );
        assert_eq!(
            merged_instructions(None, Some("chapter")).as_deref(),
            Some("chapter")
        );
        assert_eq!(
            merged_instructions(Some("global"), None).as_deref(),
            Some("global")
        );
        assert_eq!(merged_instructions(None, None), None);
    }
}
