use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use crate::*;
use koharu_scene::{Authored, EntityId, Snapshot, Translation};

struct ObservedStage {
    stage: Stage,
    trace: Arc<Mutex<Vec<(Stage, EntityId)>>>,
    active: Arc<AtomicUsize>,
    block: Option<(EntityId, Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
}

#[async_trait::async_trait]
impl crate::stages::StageProcessor for ObservedStage {
    fn model(&self) -> &'static str {
        "interactive-fixture"
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
        assert_eq!(
            self.active.fetch_add(1, Ordering::SeqCst),
            0,
            "inference must remain serialized"
        );
        self.trace.lock().unwrap().push((self.stage, input.page));
        if let Some((page, release, entered)) = &self.block
            && *page == input.page
        {
            entered.notify_one();
            release.notified().await;
        }
        self.active.fetch_sub(1, Ordering::SeqCst);
        let content = input
            .scene
            .page(input.page)?
            .text_group()?
            .unwrap()
            .text_layers()?
            .next()
            .unwrap()
            .content()?
            .id();
        Ok(input.scene.patch(|edit| {
            edit.set(
                content,
                &Translation {
                    text: Authored::user(format!("{} done", self.stage)),
                    language: None,
                },
            )?;
            Ok(())
        })?)
    }
}

struct Store(koharu_scene::Session);

#[async_trait::async_trait]
impl Committer for Store {
    async fn commit(&mut self, output: StageOutput) -> anyhow::Result<Snapshot> {
        Ok(self.0.commit(output.patch).await?.snapshot)
    }
}

#[tokio::test]
async fn remove_on_other_project_runs_between_translation_pages() {
    let (background, bg) = crate::resume::text_fixture(2).await;
    let (editing, edit) = crate::resume::text_fixture(1).await;
    let release = Arc::new(tokio::sync::Notify::new());
    let entered = Arc::new(tokio::sync::Notify::new());
    let trace = Arc::new(Mutex::new(Vec::new()));
    let active = Arc::new(AtomicUsize::new(0));
    let pipeline = Pipeline::test_processors(vec![
        (
            Stage::Translation,
            Box::new(ObservedStage {
                stage: Stage::Translation,
                trace: trace.clone(),
                active: active.clone(),
                block: Some((bg[0].0, release.clone(), entered.clone())),
            }),
        ),
        (
            Stage::Inpainting,
            Box::new(ObservedStage {
                stage: Stage::Inpainting,
                trace: trace.clone(),
                active: active.clone(),
                block: None,
            }),
        ),
    ]);
    let request = Request {
        operation: Operation::Only {
            stage: Stage::Translation,
        },
        ..Default::default()
    };
    let remove = Request {
        operation: Operation::Only {
            stage: Stage::Inpainting,
        },
        inpainting_mask: Some(InpaintingMask {
            page: edit[0].0,
            png: Arc::from([]),
        }),
        ..Default::default()
    };
    let mut background = Store(background);
    let mut editing = Store(editing);
    let background_base = background.0.snapshot().revision();
    let editing_base = editing.0.snapshot().revision();
    let mut background_run =
        Box::pin(pipeline.execute(background.0.snapshot(), request, &mut background));
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        tokio::select! {
            result = &mut background_run => panic!("background stopped before its first page: {result:?}"),
            () = entered.notified() => {}
        }
    }).await.unwrap();
    assert_eq!(*trace.lock().unwrap(), vec![(Stage::Translation, bg[0].0)]);
    let mut remove_run = Box::pin(pipeline.execute(editing.0.snapshot(), remove, &mut editing));
    assert!(futures::poll!(remove_run.as_mut()).is_pending());
    release.notify_one();
    let (background_result, remove_result) =
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            futures::join!(background_run, remove_run)
        })
        .await
        .expect("Remove must not leave either project waiting indefinitely");
    assert_eq!(background_result.unwrap().completed, 2);
    assert_eq!(remove_result.unwrap().completed, 1);
    assert_eq!(
        *trace.lock().unwrap(),
        vec![
            (Stage::Translation, bg[0].0),
            (Stage::Inpainting, edit[0].0),
            (Stage::Translation, bg[1].0),
        ],
        "Remove must finish before the next background page, rather than waiting for the entire project"
    );
    assert!(background.0.snapshot().revision() > background_base);
    assert!(editing.0.snapshot().revision() > editing_base);
}

#[tokio::test]
async fn cancelling_queued_remove_keeps_background_translation_and_releases_priority() {
    let (background, bg) = crate::resume::text_fixture(2).await;
    let (editing, edit) = crate::resume::text_fixture(1).await;
    let release = Arc::new(tokio::sync::Notify::new());
    let entered = Arc::new(tokio::sync::Notify::new());
    let trace = Arc::new(Mutex::new(Vec::new()));
    let pipeline = Pipeline::test_processors(vec![(
        Stage::Translation,
        Box::new(ObservedStage {
            stage: Stage::Translation,
            trace: trace.clone(),
            active: Arc::default(),
            block: Some((bg[0].0, release.clone(), entered.clone())),
        }),
    )]);
    let mut background = Store(background);
    let mut editing = Store(editing);
    let original = editing.0.snapshot().revision();
    let mut background_run = Box::pin(pipeline.execute(
        background.0.snapshot(),
        Request {
            operation: Operation::Only {
                stage: Stage::Translation,
            },
            ..Default::default()
        },
        &mut background,
    ));
    tokio::select! {
        result = &mut background_run => panic!("background stopped early: {result:?}"),
        () = entered.notified() => {}
    }
    let stop = StopToken::default();
    let mut remove_run = Box::pin(pipeline.execute(
        editing.0.snapshot(),
        Request {
            stop: stop.clone(),
            operation: Operation::Only {
                stage: Stage::Inpainting,
            },
            inpainting_mask: Some(InpaintingMask {
                page: edit[0].0,
                png: Arc::from([]),
            }),
            ..Default::default()
        },
        &mut editing,
    ));
    assert!(futures::poll!(remove_run.as_mut()).is_pending());
    stop.stop();
    let report = tokio::time::timeout(std::time::Duration::from_secs(1), remove_run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(report.status, RunStatus::Stopped);
    assert_eq!(report.completed, 0);
    assert_eq!(editing.0.snapshot().revision(), original);
    release.notify_one();
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), background_run)
            .await
            .unwrap()
            .unwrap()
            .completed,
        2
    );
    assert_eq!(
        *trace.lock().unwrap(),
        vec![(Stage::Translation, bg[0].0), (Stage::Translation, bg[1].0)]
    );
}

#[tokio::test]
async fn remove_and_external_chapter_summary_share_the_same_inference_lane() {
    let (editing, ids) = crate::resume::text_fixture(1).await;
    let trace = Arc::new(Mutex::new(Vec::new()));
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let pipeline = Pipeline::test_processors(vec![(
        Stage::Inpainting,
        Box::new(ObservedStage {
            stage: Stage::Inpainting,
            trace: trace.clone(),
            active: Arc::default(),
            block: Some((ids[0].0, release.clone(), entered.clone())),
        }),
    )]);
    let summary_stop = StopToken::default();
    let summary = pipeline.reserve_inference(&summary_stop).await.unwrap();
    let mut editing = Store(editing);
    let mut remove_run = Box::pin(pipeline.execute(
        editing.0.snapshot(),
        Request {
            operation: Operation::Only {
                stage: Stage::Inpainting,
            },
            inpainting_mask: Some(InpaintingMask {
                page: ids[0].0,
                png: Arc::from([]),
            }),
            ..Default::default()
        },
        &mut editing,
    ));
    assert!(futures::poll!(remove_run.as_mut()).is_pending());
    assert!(
        trace.lock().unwrap().is_empty(),
        "Remove must wait for the active chapter summary to finish"
    );
    drop(summary);
    tokio::select! {
        result = &mut remove_run => panic!("Remove stopped early: {result:?}"),
        () = entered.notified() => {}
    }
    let next_summary = pipeline.reserve_inference(&summary_stop);
    tokio::pin!(next_summary);
    assert!(
        futures::poll!(next_summary.as_mut()).is_pending(),
        "a new summary cannot overlap Remove inference"
    );
    release.notify_one();
    assert_eq!(remove_run.await.unwrap().completed, 1);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), next_summary)
            .await
            .unwrap()
            .is_some()
    );
}
