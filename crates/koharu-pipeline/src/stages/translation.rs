use anyhow::{Result, ensure};
use async_trait::async_trait;
use koharu_scene::{Authored, LanguageTag, Origin, SourceText, Translation};
use koharu_translator::{TranslationContext, TranslationRequest, Translator};

use crate::TranslationConfig;

use super::{StageInput, StageProcessor, checkpoint, generation};

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
        let batch = Batch::new(&input, self.config.target_language.tag()).await?;
        if batch.targets.is_empty() {
            return checkpoint(
                &input,
                crate::Stage::Translation,
                self.config.target_language.tag(),
                input.scene.edit_as(generation(PRODUCER, self.model())?),
            );
        }
        if batch.pending.is_empty() {
            return batch
                .finish(
                    &input,
                    self.config.model.provider.into(),
                    self.config.target_language.tag(),
                    Vec::new(),
                )
                .await;
        }
        let mut request = batch.request(self.config.target_language);
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
        batch
            .finish(
                &input,
                provider,
                self.config.target_language.tag(),
                translated,
            )
            .await
    }
}

struct Batch {
    targets: Vec<(koharu_scene::EntityId, String)>,
    pending: Vec<(koharu_scene::EntityId, String)>,
    context: Vec<TranslationContext>,
}

impl Batch {
    async fn new(input: &StageInput, language: &str) -> Result<Self> {
        let targets = targets(input, language)?;
        let saved = input.translated.lock().await;
        let mut pending = Vec::new();
        let mut context = Vec::new();
        for (entity, source) in &targets {
            if let Some(translation) = saved.get(entity) {
                context.push(TranslationContext {
                    source: source.clone(),
                    translation: translation.clone(),
                });
            } else {
                pending.push((*entity, source.clone()));
            }
        }
        Ok(Self {
            targets,
            pending,
            context,
        })
    }

    fn request(&self, language: koharu_translator::Language) -> TranslationRequest {
        let mut request = TranslationRequest::new(
            self.pending.iter().map(|(_, source)| source.clone()),
            language,
        );
        request.context = self.context.clone();
        request
    }

    async fn finish(
        self,
        input: &StageInput,
        provider: &'static str,
        language: &str,
        translated: Vec<String>,
    ) -> Result<koharu_scene::Patch> {
        ensure!(
            translated.len() == self.pending.len(),
            "translation count does not match the requested text"
        );
        let translated = {
            let mut saved = input.translated.lock().await;
            for ((entity, source), text) in self.pending.iter().zip(translated) {
                // This existing literal-pause rule must run before empty validation.
                let text = if source.trim() == "\u{2026}" {
                    "\u{2026}".to_owned()
                } else {
                    text
                };
                if !text.trim().is_empty() {
                    saved.insert(*entity, text);
                }
            }
            let missing = self
                .targets
                .iter()
                .enumerate()
                .filter(|(_, (entity, _))| !saved.contains_key(entity))
                .map(|(index, _)| index + 1)
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(koharu_translator::Error::EmptyTranslation {
                    provider,
                    segments: missing,
                }
                .into());
            }
            self.targets
                .iter()
                .map(|(entity, _)| saved[entity].clone())
                .collect::<Vec<_>>()
        };
        let targets = self.targets;
        let tag = language;
        let language = LanguageTag::new(language)?;
        let generated = generation(PRODUCER, provider)?;
        let mut edit = input.scene.edit_as(generated.clone());
        for (entity, _) in &targets {
            edit.observe::<SourceText>(*entity)?;
            edit.observe::<Translation>(*entity)?;
        }
        for ((entity, _source), text) in targets.into_iter().zip(translated) {
            if input
                .scene
                .component::<Translation>(entity)?
                .is_some_and(|value| matches!(value.text.origin, Origin::User))
            {
                continue;
            }
            edit.set(
                entity,
                &Translation {
                    text: Authored::generated(text, generated.clone()),
                    language: Some(language.clone()),
                },
            )?;
        }
        checkpoint(input, crate::Stage::Translation, tag, edit)
    }
}

fn targets(input: &StageInput, language: &str) -> Result<Vec<(koharu_scene::EntityId, String)>> {
    let refresh = input.resume
        && crate::resume::changed(
            &input.scene,
            input.page,
            crate::Stage::Translation,
            language,
        )?;
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
            if source.text.value.trim().is_empty() {
                continue;
            }
            if let Some(previous) = input.scene.component::<Translation>(content.id())? {
                if matches!(previous.text.origin, Origin::User)
                    || (input.resume && !refresh && crate::resume::translated(&previous, language))
                {
                    continue;
                }
            }
            targets.push((content.id(), source.text.value));
        }
    }
    Ok(targets)
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

    #[tokio::test]
    async fn empty_translation_never_commits_or_marks_a_page_complete_before_recovery() {
        use super::{Batch, StageInput};
        use koharu_scene::{Translation, TranslationCheckpoint};
        use std::sync::Arc;
        let (mut session, ids) = crate::resume::text_fixture(1).await;
        let input = StageInput::new(
            session.snapshot(),
            ids[0].0,
            None,
            None,
            Arc::default(),
            None,
        )
        .with_resume(true);
        for empty in ["", " ", "\n\t"] {
            let batch = Batch::new(&input, "en").await.unwrap();
            assert_eq!(
                batch.request(koharu_translator::Language::English).segments,
                vec!["source 0"]
            );
            let error = batch
                .finish(&input, "openai-compatible", "en", vec![empty.into()])
                .await
                .unwrap_err();
            assert!(
                error
                    .downcast_ref::<koharu_translator::Error>()
                    .unwrap()
                    .retry_delay(std::time::Duration::from_secs(15))
                    .is_some()
            );
            assert!(
                session
                    .snapshot()
                    .component::<Translation>(ids[0].1)
                    .unwrap()
                    .is_none()
            );
            assert!(
                session
                    .snapshot()
                    .component::<TranslationCheckpoint>(ids[0].0)
                    .unwrap()
                    .is_none()
            );
        }
        let different_job = StageInput::new(
            session.snapshot(),
            ids[0].0,
            None,
            None,
            Arc::default(),
            None,
        );
        assert_eq!(
            Batch::new(&different_job, "en")
                .await
                .unwrap()
                .request(koharu_translator::Language::English)
                .segments
                .len(),
            1
        );
        let batch = Batch::new(&input, "en").await.unwrap();
        assert!(
            batch
                .finish(&input, "openai-compatible", "en", vec![])
                .await
                .is_err()
        );
        let batch = Batch::new(&input, "en").await.unwrap();
        let patch = batch
            .finish(&input, "openai-compatible", "en", vec!["recovered".into()])
            .await
            .unwrap();
        session.commit(patch).await.unwrap();
        assert!(
            crate::resume::complete(
                &session.snapshot(),
                ids[0].0,
                crate::Stage::Translation,
                "en"
            )
            .unwrap()
        );
    }

    #[tokio::test]
    async fn empty_translation_for_literal_ellipsis_preserves_the_existing_pause() {
        use super::{Batch, StageInput};
        use koharu_scene::{Authored, SourceText, Translation};
        use std::sync::Arc;
        let (mut session, ids) = crate::resume::text_fixture(1).await;
        let mut edit = session.snapshot().edit();
        edit.set(
            ids[0].1,
            &SourceText {
                text: Authored::user("…".into()),
                language: None,
            },
        )
        .unwrap();
        session.commit(edit.finish().unwrap()).await.unwrap();
        let input = StageInput::new(
            session.snapshot(),
            ids[0].0,
            None,
            None,
            Arc::default(),
            None,
        );
        let patch = Batch::new(&input, "en")
            .await
            .unwrap()
            .finish(&input, "openai-compatible", "en", vec![String::new()])
            .await
            .unwrap();
        session.commit(patch).await.unwrap();
        assert_eq!(
            session
                .snapshot()
                .component::<Translation>(ids[0].1)
                .unwrap()
                .unwrap()
                .text
                .value,
            "…"
        );
    }

    #[tokio::test]
    async fn empty_translation_recovers_only_missing_segments_in_the_same_job() {
        use super::{Batch, StageInput, StageProcessor};
        use koharu_scene::{
            At, Authored, Origin, SourceText, TextLayout, TextLayoutKind, Translation,
        };
        use std::sync::{Arc, Mutex};
        let (mut session, ids) = crate::resume::text_fixture(3).await;
        let mut edit = session.snapshot().edit();
        let second = edit.add_text_content(ids[1].0, At::End).unwrap();
        edit.set(
            second,
            &SourceText {
                text: Authored::user("second segment".into()),
                language: None,
            },
        )
        .unwrap();
        edit.add_text_layer(
            ids[1].0,
            At::End,
            second,
            &TextLayout {
                origin: Origin::User,
                kind: TextLayoutKind::Paragraph,
                angle_degrees: None,
            },
        )
        .unwrap();
        session.commit(edit.finish().unwrap()).await.unwrap();
        struct EmptyOnce {
            calls: Arc<Mutex<Vec<Vec<String>>>>,
        }
        #[async_trait::async_trait]
        impl StageProcessor for EmptyOnce {
            fn model(&self) -> &'static str {
                "empty-output-fixture"
            }
            fn unload(&self) -> bool {
                true
            }
            async fn load(&self) -> anyhow::Result<()> {
                Ok(())
            }
            async fn process(&self, input: StageInput) -> anyhow::Result<koharu_scene::Patch> {
                let batch = Batch::new(&input, "en").await?;
                let request = batch.request(koharu_translator::Language::English);
                let call = {
                    let mut calls = self.calls.lock().unwrap();
                    let index = calls.len();
                    calls.push(request.segments.clone());
                    index
                };
                if call == 2 {
                    assert_eq!(
                        request.context,
                        vec![koharu_translator::TranslationContext {
                            source: "source 1".into(),
                            translation: "saved second page".into()
                        }]
                    );
                }
                let output = if call == 1 {
                    vec!["saved second page".into(), String::new()]
                } else {
                    request
                        .segments
                        .iter()
                        .map(|_| format!("saved call {call}"))
                        .collect()
                };
                batch
                    .finish(&input, "openai-compatible", "en", output)
                    .await
            }
        }
        struct Store<'a>(&'a mut koharu_scene::Session);
        #[async_trait::async_trait]
        impl crate::Committer for Store<'_> {
            async fn commit(
                &mut self,
                output: crate::StageOutput,
            ) -> anyhow::Result<koharu_scene::Snapshot> {
                Ok(self.0.commit(output.patch).await?.snapshot)
            }
        }
        let calls = Arc::new(Mutex::new(Vec::new()));
        let pipeline = crate::Pipeline::test_translation(Box::new(EmptyOnce {
            calls: calls.clone(),
        }));
        let result = pipeline
            .execute(
                session.snapshot(),
                crate::Request {
                    resume: true,
                    operation: crate::Operation::Only {
                        stage: crate::Stage::Translation,
                    },
                    ..Default::default()
                },
                &mut Store(&mut session),
            )
            .await;
        assert!(
            result.is_ok(),
            "empty model text should recover without manual Run from here: {result:?}"
        );
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                vec!["source 0"],
                vec!["source 1", "second segment"],
                vec!["second segment"],
                vec!["source 2"]
            ]
        );
        for (content, expected) in [
            (ids[0].1, "saved call 0"),
            (ids[1].1, "saved second page"),
            (second, "saved call 2"),
            (ids[2].1, "saved call 3"),
        ] {
            assert_eq!(
                session
                    .snapshot()
                    .component::<Translation>(content)
                    .unwrap()
                    .unwrap()
                    .text
                    .value,
                expected
            );
        }
        pipeline
            .execute(
                session.snapshot(),
                crate::Request {
                    operation: crate::Operation::Only {
                        stage: crate::Stage::Translation,
                    },
                    ..Default::default()
                },
                &mut Store(&mut session),
            )
            .await
            .unwrap();
        assert_eq!(
            &calls.lock().unwrap()[4..],
            &[
                vec!["source 0".to_owned()],
                vec!["source 1".to_owned(), "second segment".to_owned()],
                vec!["source 2".to_owned()]
            ],
            "a new explicit rerun must not reuse the previous job's transient cache"
        );
    }

    #[tokio::test]
    async fn resume_only_requests_missing_text_and_preserves_user_edits() {
        use koharu_scene::{
            At, Authored, Generation, LanguageTag, Origin, ProducerId, SourceText, TextLayout,
            TextLayoutKind, Translation,
        };
        use std::sync::Arc;
        let (mut session, ids) = crate::resume::text_fixture(1).await;
        let (page, done) = ids[0];
        let generated = Generation::new(ProducerId::new(super::PRODUCER).unwrap());
        let mut edit = session.snapshot().edit();
        let mut contents = vec![done];
        for source in ["missing", "wrong language", "empty result", "user edit"] {
            let content = edit.add_text_content(page, At::End).unwrap();
            edit.set(
                content,
                &SourceText {
                    text: Authored::user(source.into()),
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
            contents.push(content);
        }
        session.commit(edit.finish().unwrap()).await.unwrap();
        let mut edit = session.snapshot().edit_as(generated.clone());
        for (index, language, text) in [(0, "vi", "saved"), (2, "en", "wrong"), (3, "vi", "")] {
            edit.set(
                contents[index],
                &Translation {
                    text: Authored::generated(text.into(), generated.clone()),
                    language: Some(LanguageTag::new(language).unwrap()),
                },
            )
            .unwrap();
        }
        session.commit(edit.finish().unwrap()).await.unwrap();
        let mut edit = session.snapshot().edit();
        edit.set(
            contents[4],
            &Translation {
                text: Authored::user("edited".into()),
                language: None,
            },
        )
        .unwrap();
        session.commit(edit.finish().unwrap()).await.unwrap();
        let input =
            super::StageInput::new(session.snapshot(), page, None, None, Arc::default(), None);
        let resumed = super::targets(&input.clone().with_resume(true), "vi").unwrap();
        assert_eq!(
            resumed.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            contents[1..4]
        );
        let rerun = super::targets(&input, "vi").unwrap();
        assert_eq!(
            rerun.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            contents[..4]
        );
    }

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
