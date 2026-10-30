use super::*;

#[tokio::test]
async fn automatic_recovery_is_bounded_and_stops_for_quota_or_invalid_output() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Broken {
        calls: Arc<AtomicUsize>,
        status: u16,
        message: &'static str,
    }
    #[async_trait::async_trait]
    impl crate::stages::StageProcessor for Broken {
        fn model(&self) -> &'static str {
            "broken-proxy-fixture"
        }
        fn unload(&self) -> bool {
            true
        }
        async fn load(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn process(
            &self,
            _: crate::stages::StageInput,
        ) -> anyhow::Result<koharu_scene::Patch> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(koharu_translator::Error::Api {
                provider: "openai-compatible",
                status: self.status,
                message: self.message.into(),
            }
            .into())
        }
    }
    for (status, message, expected_calls, stop_on_retry) in [
        (
            502,
            r#"{"error":{"type":"koharu_proxy_error","code":"upstream_unreachable"}}"#,
            4,
            false,
        ),
        (429, "insufficient_quota", 1, false),
        (
            502,
            r#"{"error":{"type":"koharu_proxy_error","code":"invalid_translation_json"}}"#,
            1,
            false,
        ),
        (
            502,
            r#"{"error":{"type":"koharu_proxy_error","code":"upstream_unreachable"}}"#,
            1,
            true,
        ),
        (
            524,
            r#"{"title":"Error 524: A timeout occurred","status":524,"detail":"The origin web server did not return a complete response within the 120-second Proxy Read Timeout window."}"#,
            1,
            true,
        ),
    ] {
        let (session, _) = crate::resume::text_fixture(1).await;
        let revision = session.snapshot().revision();
        let calls = Arc::new(AtomicUsize::new(0));
        let pipeline = Pipeline::test_translation(Box::new(Broken {
            calls: calls.clone(),
            status,
            message,
        }));
        let stop = StopToken::default();
        let outcome_stop = stop.clone();
        let progress_stop = stop.clone();
        let retries = Arc::new(std::sync::Mutex::new(Vec::new()));
        let progress_retries = retries.clone();
        let request = Request {
            resume: true,
            stop,
            operation: Operation::Only {
                stage: Stage::Translation,
            },
            progress: Some(Arc::new(move |event| {
                if let Progress::Retrying { retry, .. } = event {
                    progress_retries.lock().unwrap().push(retry.delay_seconds);
                    if stop_on_retry {
                        progress_stop.stop();
                    }
                }
            })),
            ..Request::default()
        };
        let result = pipeline
            .execute(session.snapshot(), request, &mut RejectCommitter)
            .await;
        assert_eq!(
            calls.load(Ordering::SeqCst),
            expected_calls,
            "{status} {message}"
        );
        if stop_on_retry {
            assert_eq!(result.unwrap().status, RunStatus::Stopped);
            if status == 524 {
                assert_eq!(*retries.lock().unwrap(), vec![120]);
            }
        } else {
            assert!(result.is_err());
            assert!(
                outcome_stop.stopped(),
                "a terminal failure must cancel waiting sibling stages without turning into a successful Stop report"
            );
        }
        assert_eq!(session.snapshot().revision(), revision);
    }
}

#[tokio::test]
async fn automatic_recovery_continues_the_failed_page_without_manual_resume() {
    use koharu_scene::{Authored, Generation, LanguageTag, ProducerId, Translation};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct Interrupted {
        calls: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl crate::stages::StageProcessor for Interrupted {
        fn model(&self) -> &'static str {
            "interrupted-proxy-fixture"
        }
        fn unload(&self) -> bool {
            true
        }
        async fn load(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn process(
            &self,
            input: crate::stages::StageInput,
        ) -> anyhow::Result<koharu_scene::Patch> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 1 {
                return Err(koharu_translator::Error::Api {
                    provider: "openai-compatible",
                    status: 502,
                    message:
                        r#"{"error":{"type":"koharu_proxy_error","code":"upstream_unreachable"}}"#
                            .into(),
                }
                .into());
            }
            let generated = Generation::new(ProducerId::new("dev.koharu.pipeline.translation")?);
            let mut edit = input.scene.edit_as(generated.clone());
            for layer in input
                .scene
                .page(input.page)?
                .text_group()?
                .unwrap()
                .text_layers()?
            {
                edit.set(
                    layer.content()?.id(),
                    &Translation {
                        text: Authored::generated(format!("call {call}"), generated.clone()),
                        language: Some(LanguageTag::new(
                            koharu_translator::Language::English.tag(),
                        )?),
                    },
                )?;
            }
            crate::resume::record(
                &input,
                Stage::Translation,
                koharu_translator::Language::English.tag(),
                &mut edit,
            )?;
            Ok(edit.finish()?)
        }
    }
    struct Store<'a>(&'a mut koharu_scene::Session);
    #[async_trait::async_trait]
    impl Committer for Store<'_> {
        async fn commit(&mut self, output: StageOutput) -> anyhow::Result<koharu_scene::Snapshot> {
            Ok(self.0.commit(output.patch).await?.snapshot)
        }
    }
    let (mut session, ids) = crate::resume::text_fixture(3).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let pipeline = Pipeline::test_translation(Box::new(Interrupted {
        calls: calls.clone(),
    }));
    let result = pipeline
        .execute(
            session.snapshot(),
            Request {
                resume: true,
                operation: Operation::Only {
                    stage: Stage::Translation,
                },
                ..Request::default()
            },
            &mut Store(&mut session),
        )
        .await;
    assert!(
        result.is_ok(),
        "a temporary proxy failure must recover in the same job: {result:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    for (index, expected) in ["call 0", "call 2", "call 3"].iter().enumerate() {
        assert_eq!(
            session
                .snapshot()
                .component::<Translation>(ids[index].1)
                .unwrap()
                .unwrap()
                .text
                .value,
            *expected
        );
    }
}

#[tokio::test]
async fn resume_after_quota_failure_skips_committed_pages_and_keeps_pending_work() {
    use koharu_scene::{Authored, Generation, LanguageTag, ProducerId, Translation};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    struct Quota {
        calls: Arc<AtomicUsize>,
        exhausted: Arc<AtomicBool>,
    }
    #[async_trait::async_trait]
    impl crate::stages::StageProcessor for Quota {
        fn model(&self) -> &'static str {
            "quota-fixture"
        }
        fn unload(&self) -> bool {
            true
        }
        async fn load(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn process(
            &self,
            input: crate::stages::StageInput,
        ) -> anyhow::Result<koharu_scene::Patch> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 1 && self.exhausted.load(Ordering::SeqCst) {
                anyhow::bail!("HTTP 429 quota exhausted")
            }
            let generation = Generation::new(ProducerId::new("dev.koharu.pipeline.translation")?);
            let mut edit = input.scene.edit_as(generation.clone());
            for layer in input
                .scene
                .page(input.page)?
                .text_group()?
                .unwrap()
                .text_layers()?
            {
                edit.set(
                    layer.content()?.id(),
                    &Translation {
                        text: Authored::generated(format!("call {call}"), generation.clone()),
                        language: Some(LanguageTag::new(
                            koharu_translator::Language::English.tag(),
                        )?),
                    },
                )?;
            }
            crate::resume::record(
                &input,
                Stage::Translation,
                koharu_translator::Language::English.tag(),
                &mut edit,
            )?;
            Ok(edit.finish()?)
        }
    }
    struct Store<'a>(&'a mut koharu_scene::Session);
    #[async_trait::async_trait]
    impl Committer for Store<'_> {
        async fn commit(&mut self, output: StageOutput) -> anyhow::Result<koharu_scene::Snapshot> {
            Ok(self.0.commit(output.patch).await?.snapshot)
        }
    }
    let (mut session, ids) = crate::resume::text_fixture(3).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let exhausted = Arc::new(AtomicBool::new(true));
    let pipeline = Pipeline::test_translation(Box::new(Quota {
        calls: calls.clone(),
        exhausted: exhausted.clone(),
    }));
    let request = || Request {
        resume: true,
        operation: Operation::Only {
            stage: Stage::Translation,
        },
        ..Request::default()
    };
    let result = pipeline
        .execute(session.snapshot(), request(), &mut Store(&mut session))
        .await;
    assert!(result.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(
        session
            .snapshot()
            .component::<Translation>(ids[0].1)
            .unwrap()
            .is_some()
    );
    assert!(
        session
            .snapshot()
            .component::<Translation>(ids[1].1)
            .unwrap()
            .is_none()
    );
    let pending = pipeline
        .pending_pages(
            &session.snapshot(),
            &ids.iter().map(|id| id.0).collect::<Vec<_>>(),
        )
        .unwrap();
    assert_eq!(pending, vec![ids[1].0, ids[2].0]);
    exhausted.store(false, Ordering::SeqCst);
    pipeline
        .execute(session.snapshot(), request(), &mut Store(&mut session))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    assert_eq!(
        session
            .snapshot()
            .component::<Translation>(ids[0].1)
            .unwrap()
            .unwrap()
            .text
            .value,
        "call 0"
    );
    assert!(
        pipeline
            .pending_pages(
                &session.snapshot(),
                &ids.iter().map(|id| id.0).collect::<Vec<_>>()
            )
            .unwrap()
            .is_empty()
    );
    pipeline
        .execute(session.snapshot(), request(), &mut Store(&mut session))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    let rerun = Request {
        operation: Operation::Only {
            stage: Stage::Translation,
        },
        ..Request::default()
    };
    pipeline
        .execute(session.snapshot(), rerun, &mut Store(&mut session))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 7);
}

#[test]
fn configuration_ignores_unknown_fields() {
    let config = toml::from_str::<PipelineConfig>("legacy_limit = 1").unwrap();
    assert_eq!(config, PipelineConfig::default());
}

#[tokio::test]
async fn stop_is_a_successful_partial_result() {
    let pipeline = pipeline(Default::default());
    let stop = StopToken::default();
    stop.stop();
    let request = Request {
        stop,
        ..Request::default()
    };
    let mut committer = RejectCommitter;

    let report = pipeline
        .execute(
            koharu_scene::Session::memory().await.unwrap().snapshot(),
            request,
            &mut committer,
        )
        .await
        .unwrap();

    assert_eq!(report.status, RunStatus::Stopped);
    assert_eq!(report.completed, 0);
}

#[tokio::test]
async fn stop_after_a_page_keeps_completed_progress() {
    let translation = TranslationConfig {
        model: koharu_translator::ModelSelection {
            provider: koharu_translator::Provider::OpenAi,
            model: Some("gpt-5.6-luna".to_owned()),
            quantization: None,
            vision: true,
            reasoning: true,
        },
        ..Default::default()
    };
    let pipeline = pipeline(translation);
    let mut session = koharu_scene::Session::memory().await.unwrap();
    let patch = session
        .snapshot()
        .patch(|edit| {
            edit.add_page(
                koharu_scene::PageDraft::new("one", 1.0, 1.0),
                koharu_scene::At::End,
            )?;
            edit.add_page(
                koharu_scene::PageDraft::new("two", 1.0, 1.0),
                koharu_scene::At::End,
            )?;
            Ok(())
        })
        .unwrap();
    session.commit(patch).await.unwrap();
    let stop = StopToken::default();
    let progress_stop = stop.clone();
    let request = Request {
        operation: Operation::Only {
            stage: Stage::Translation,
        },
        stop,
        progress: Some(std::sync::Arc::new(move |event| {
            if matches!(event, Progress::Skipped { .. }) {
                progress_stop.stop();
            }
        })),
        ..Request::default()
    };
    let mut committer = RejectCommitter;

    let report = pipeline
        .execute(session.snapshot(), request, &mut committer)
        .await
        .unwrap();

    assert_eq!(report.status, RunStatus::Stopped);
    assert_eq!(report.completed, 1);
    assert_eq!(report.total, 2);
}

struct RejectCommitter;

fn pipeline(translation: TranslationConfig) -> Pipeline {
    let config = PipelineConfig {
        translation,
        ..PipelineConfig::default()
    };
    Pipeline::from_config(
        koharu_config::Config::memory(config),
        koharu_config::Config::memory(koharu_translator::ProvidersConfig::default()),
        koharu_ml::Device::cpu(),
    )
    .unwrap()
}

#[async_trait::async_trait]
impl Committer for RejectCommitter {
    async fn commit(&mut self, _output: StageOutput) -> anyhow::Result<koharu_scene::Snapshot> {
        anyhow::bail!("stopped execution must not commit")
    }
}

#[test]
fn operations_expand_to_the_supported_workflows() {
    assert_eq!(
        Operation::Through {
            stage: Stage::Translation,
        }
        .stages()
        .unwrap(),
        vec![Stage::Detection, Stage::Ocr, Stage::Translation],
    );
    assert_eq!(
        Operation::Through {
            stage: Stage::Inpainting,
        }
        .stages()
        .unwrap(),
        vec![Stage::Detection, Stage::Inpainting],
    );
    assert_eq!(
        Operation::Only {
            stage: Stage::Translation,
        }
        .stages()
        .unwrap(),
        vec![Stage::Translation],
    );
    assert_eq!(
        Operation::Stages {
            stages: vec![Stage::Translation, Stage::Detection, Stage::Translation],
        }
        .stages()
        .unwrap(),
        vec![Stage::Detection, Stage::Translation],
    );
}
