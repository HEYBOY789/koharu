use std::sync::Arc;

use anyhow::{Context as _, bail};
use image::{GrayImage, ImageEncoder as _, codecs::png::PngEncoder};
use koharu_desktop::{CanvasState, Desktop, Frame, TransformFrame};
use koharu_rasterizer::ResourceId;
use koharu_scene::{EntityId, Revision};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{
    AppHandle, State,
    ipc::{Channel, IpcResponse},
};
use tauri_runtime_cef::CefRuntime;

use super::{
    ChannelExt as _, Error, processing,
    processing::JobId,
    project::{
        CurrentProject, Page, Project, ProjectLease, ProjectOwner, ProjectWorkspace,
        RasterStrokeMode,
    },
};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize, Type)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

#[derive(Clone, Copy, Debug, Deserialize, Type)]
pub struct PaintBrush {
    pub diameter: f32,
    pub color: [u8; 4],
}

#[derive(Clone, Copy, Debug, Serialize, Type)]
pub struct LayerCommit {
    pub revision: Revision,
    pub layer: EntityId,
}

#[derive(Type)]
#[specta(transparent)]
pub(crate) struct CanvasBytes(#[specta(type = Vec<u8>)] Vec<u8>);

#[derive(Clone, Copy, Deserialize, Type)]
#[specta(transparent)]
pub(crate) struct CanvasGeneration(#[specta(type = f64)] u64);

#[derive(Clone, Debug, Serialize, Type)]
pub struct CanvasPagePreparation {
    pub revision: Revision,
    pub page: Page,
}

impl IpcResponse for CanvasBytes {
    fn body(self) -> tauri::Result<tauri::ipc::InvokeResponseBody> {
        Ok(self.0.into())
    }
}

#[derive(Default)]
pub(crate) struct CanvasChannel {
    pub(crate) channel: Mutex<Option<Channel<CanvasState>>>,
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_canvas_manifest(
    generation: CanvasGeneration,
    desktop: State<'_, Desktop>,
) -> Result<CanvasBytes, Error> {
    Ok(CanvasBytes(desktop.frame_manifest_bytes(generation.0)?))
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_canvas_resource(
    generation: CanvasGeneration,
    resource: String,
    desktop: State<'_, Desktop>,
) -> Result<CanvasBytes, Error> {
    let resource = resource
        .parse::<ResourceId>()
        .context("canvas resource id is invalid")?;
    Ok(CanvasBytes(
        desktop.frame_resource_bytes(generation.0, resource)?,
    ))
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn prepare_canvas_page(
    page: EntityId,
    desktop: State<'_, Desktop>,
    project: State<'_, CurrentProject>,
) -> Result<Option<CanvasPagePreparation>, Error> {
    let (snapshot, prepared_page) = {
        let project = project.project.lock().await;
        let project = project.as_ref().context("no project is open")?;
        let snapshot = project.snapshot();
        let prepared_page = Project::page(&snapshot, page)?;
        (snapshot, prepared_page)
    };
    let revision = snapshot.revision();
    Ok(desktop
        .prepare_page(&snapshot, page)
        .await?
        .then_some(CanvasPagePreparation {
            revision,
            page: prepared_page,
        }))
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_canvas_page_manifest(
    page: EntityId,
    revision: Revision,
    desktop: State<'_, Desktop>,
) -> Result<CanvasBytes, Error> {
    Ok(CanvasBytes(desktop.page_manifest_bytes(page, revision)?))
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_canvas_page_resource(
    page: EntityId,
    revision: Revision,
    resource: String,
    desktop: State<'_, Desktop>,
) -> Result<CanvasBytes, Error> {
    let resource = resource
        .parse::<ResourceId>()
        .context("canvas resource id is invalid")?;
    Ok(CanvasBytes(
        desktop.page_resource_bytes(page, revision, resource)?,
    ))
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn add_point_text(
    point: Point,
    desktop: State<'_, Desktop>,
    project: State<'_, CurrentProject>,
    canvas_channel: State<'_, CanvasChannel>,
) -> Result<LayerCommit, Error> {
    let (commit, page, layer) = {
        let mut project = project.project.lock().await;
        let project = project.as_mut().context("no project is open")?;
        let page = project
            .active_page()
            .context("the project has no active page")?;
        let (commit, layer) = project.add_point_text(page, point).await?;
        project.record_commit(&commit);
        (commit, project.active_page(), layer)
    };
    desktop.synchronize(&commit.snapshot, page, &commit).await?;
    canvas_channel.channel.publish(desktop.canvas_state());
    Ok(LayerCommit {
        revision: commit.revision,
        layer,
    })
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn add_text_box(
    frame: Frame,
    desktop: State<'_, Desktop>,
    project: State<'_, CurrentProject>,
    canvas_channel: State<'_, CanvasChannel>,
) -> Result<LayerCommit, Error> {
    let (commit, page, layer) = {
        let mut project = project.project.lock().await;
        let project = project.as_mut().context("no project is open")?;
        let page = project
            .active_page()
            .context("the project has no active page")?;
        let (commit, layer) = project.add_text_box(page, frame).await?;
        project.record_commit(&commit);
        (commit, project.active_page(), layer)
    };
    desktop.synchronize(&commit.snapshot, page, &commit).await?;
    canvas_channel.channel.publish(desktop.canvas_state());
    Ok(LayerCommit {
        revision: commit.revision,
        layer,
    })
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn commit_paint(
    expected_revision: Revision,
    layer: Option<EntityId>,
    points: Vec<Point>,
    brush: PaintBrush,
    desktop: State<'_, Desktop>,
    project: State<'_, CurrentProject>,
    canvas_channel: State<'_, CanvasChannel>,
) -> Result<LayerCommit, Error> {
    commit_raster_stroke(
        expected_revision,
        layer,
        points,
        brush.diameter,
        brush.color,
        RasterStrokeMode::Paint,
        &desktop,
        &project,
        &canvas_channel,
    )
    .await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn commit_erase(
    expected_revision: Revision,
    layer: EntityId,
    points: Vec<Point>,
    diameter: f32,
    desktop: State<'_, Desktop>,
    project: State<'_, CurrentProject>,
    canvas_channel: State<'_, CanvasChannel>,
) -> Result<LayerCommit, Error> {
    commit_raster_stroke(
        expected_revision,
        Some(layer),
        points,
        diameter,
        [0; 4],
        RasterStrokeMode::Erase,
        &desktop,
        &project,
        &canvas_channel,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn commit_raster_stroke(
    expected_revision: Revision,
    layer: Option<EntityId>,
    points: Vec<Point>,
    diameter: f32,
    color: [u8; 4],
    mode: RasterStrokeMode,
    desktop: &Desktop,
    project: &CurrentProject,
    canvas_channel: &CanvasChannel,
) -> Result<LayerCommit, Error> {
    let (commit, page, element) = {
        let mut project = project.project.lock().await;
        let project = project.as_mut().context("no project is open")?;
        ensure_revision(project.snapshot().revision(), expected_revision)?;
        let page = project
            .active_page()
            .context("the project has no active page")?;
        let (commit, element) = project
            .apply_raster_stroke(
                page,
                layer,
                mode,
                color,
                diameter,
                points
                    .into_iter()
                    .map(|point| koharu_scene::Point {
                        x: point.x,
                        y: point.y,
                    })
                    .collect(),
            )
            .await?;
        project.record_commit(&commit);
        (commit, project.active_page(), element)
    };
    desktop.synchronize(&commit.snapshot, page, &commit).await?;
    canvas_channel.channel.publish(desktop.canvas_state());
    Ok(LayerCommit {
        revision: commit.revision,
        layer: element,
    })
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn commit_transform(
    expected_revision: Revision,
    elements: Vec<TransformFrame>,
    desktop: State<'_, Desktop>,
    project: State<'_, CurrentProject>,
    canvas_channel: State<'_, CanvasChannel>,
) -> Result<Option<Revision>, Error> {
    let geometries = desktop.transform_geometries(expected_revision, &elements)?;
    if geometries.is_empty() {
        return Ok(None);
    }
    let (commit, page) = {
        let mut project = project.project.lock().await;
        let project = project.as_mut().context("no project is open")?;
        ensure_revision(project.snapshot().revision(), expected_revision)?;
        let commit = project.set_geometries(geometries).await?;
        project.record_commit(&commit);
        (commit, project.active_page())
    };
    desktop.synchronize(&commit.snapshot, page, &commit).await?;
    canvas_channel.channel.publish(desktop.canvas_state());
    Ok(Some(commit.revision))
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn commit_inpaint(
    expected_revision: Revision,
    points: Vec<Point>,
    diameter: f32,
    handle: AppHandle<CefRuntime>,
    project: State<'_, CurrentProject>,
) -> Result<Option<JobId>, Error> {
    let (target, request) =
        prepare_inpaint(&project.project, expected_revision, points, diameter).await?;
    Ok(Some(processing::start_process(handle, target, request)))
}

async fn prepare_inpaint(
    projects: &ProjectWorkspace,
    expected_revision: Revision,
    points: Vec<Point>,
    diameter: f32,
) -> anyhow::Result<(ProjectLease, koharu_pipeline::Request)> {
    if !diameter.is_finite() || diameter <= 0.0 || points.is_empty() {
        bail!("an inpaint stroke requires a positive diameter and at least one point");
    }
    if points
        .iter()
        .any(|point| !point.x.is_finite() || !point.y.is_finite())
    {
        bail!("inpaint stroke points must be finite");
    }
    let (origin, page, width, height) = {
        let project = projects.lock().await;
        let project = project.as_ref().context("no project is open")?;
        let snapshot = project.snapshot();
        ensure_revision(snapshot.revision(), expected_revision)?;
        let page = project
            .active_page()
            .context("the project has no active page")?;
        let page_value = snapshot.page(page)?.page()?;
        (
            project.instance_id,
            page,
            page_value.width.round() as u32,
            page_value.height.round() as u32,
        )
    };
    let target = projects.retain(origin, ProjectOwner::Pipeline).await?;
    let mask = tokio_rayon::spawn(move || encode_mask(width, height, &points, diameter)).await;
    let (png, bounds) = match mask {
        Ok(mask) => mask,
        Err(error) => {
            projects.finish(&target).await;
            return Err(error);
        }
    };
    Ok((
        target,
        koharu_pipeline::Request {
            scope: koharu_pipeline::Scope::Region { page, bounds },
            operation: koharu_pipeline::Operation::Only {
                stage: koharu_pipeline::Stage::Inpainting,
            },
            inpainting_mask: Some(koharu_pipeline::InpaintingMask {
                page,
                png: Arc::from(png),
            }),
            ..Default::default()
        },
    ))
}

fn ensure_revision(actual: Revision, expected: Revision) -> anyhow::Result<()> {
    if actual != expected {
        bail!("canvas edit expected revision {expected}, but the project is at {actual}");
    }
    Ok(())
}

fn encode_mask(
    width: u32,
    height: u32,
    points: &[Point],
    diameter: f32,
) -> anyhow::Result<(Vec<u8>, koharu_pipeline::Bounds)> {
    if width == 0 || height == 0 {
        bail!("page dimensions must be positive");
    }
    let mut image = GrayImage::new(width, height);
    let radius = f64::from(diameter) * 0.5;
    let mut dirty = None::<[u32; 4]>;
    for (start, end) in points
        .iter()
        .zip(points.iter().skip(1))
        .chain(points.last().map(|point| (point, point)))
    {
        let left = (start.x.min(end.x) - radius - 0.5).floor().max(0.0) as u32;
        let top = (start.y.min(end.y) - radius - 0.5).floor().max(0.0) as u32;
        let right = (start.x.max(end.x) + radius + 0.5)
            .ceil()
            .min(f64::from(width)) as u32;
        let bottom = (start.y.max(end.y) + radius + 0.5)
            .ceil()
            .min(f64::from(height)) as u32;
        let dx = end.x - start.x;
        let dy = end.y - start.y;
        let length_squared = dx.mul_add(dx, dy * dy);
        for y in top..bottom {
            for x in left..right {
                let px = f64::from(x) + 0.5;
                let py = f64::from(y) + 0.5;
                let progress = if length_squared <= f64::EPSILON {
                    0.0
                } else {
                    (((px - start.x) * dx + (py - start.y) * dy) / length_squared).clamp(0.0, 1.0)
                };
                let nearest_x = start.x + progress * dx;
                let nearest_y = start.y + progress * dy;
                let distance = (px - nearest_x).hypot(py - nearest_y);
                let coverage = ((radius + 0.5 - distance).clamp(0.0, 1.0) * 255.0).round() as u8;
                if coverage == 0 {
                    continue;
                }
                let pixel = image.get_pixel_mut(x, y);
                pixel.0[0] = pixel.0[0].max(coverage);
                dirty = Some(match dirty {
                    Some([min_x, min_y, max_x, max_y]) => {
                        [min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y)]
                    }
                    None => [x, y, x, y],
                });
            }
        }
    }
    let [left, top, right, bottom] = dirty.context("inpaint stroke does not intersect the page")?;
    let mut png = Vec::new();
    PngEncoder::new(&mut png).write_image(
        image.as_raw(),
        width,
        height,
        image::ExtendedColorType::L8,
    )?;
    Ok((
        png,
        koharu_pipeline::Bounds {
            x: f64::from(left),
            y: f64::from(top),
            width: f64::from(right - left + 1),
            height: f64::from(bottom - top + 1),
        },
    ))
}

#[cfg(test)]
mod interactive_tests {
    use super::*;

    async fn page_project(root: &std::path::Path, name: &str) -> Project {
        let mut project = Project::create(name.into(), root.join(format!("{name}.khrproj")))
            .await
            .unwrap();
        let mut page = None;
        let patch = project
            .snapshot()
            .patch(|edit| {
                page = Some(edit.add_page(
                    koharu_scene::PageDraft::new("001", 64.0, 64.0),
                    koharu_scene::At::End,
                )?);
                Ok(())
            })
            .unwrap();
        project.session.commit(patch).await.unwrap();
        project.active_page = page;
        project
    }

    #[tokio::test]
    async fn remove_paint_and_eraser_stay_on_editing_project_while_another_translates() {
        use crate::commands::processing::Processing;
        use koharu_scene::{AssetRole, Point as ScenePoint};
        let root = tempfile::tempdir().unwrap();
        let projects = ProjectWorkspace::default();
        let background = page_project(root.path(), "Background").await;
        let background_id = background.instance_id;
        let background_revision = background.revision();
        projects.replace(background).await;
        let background = projects
            .retain(background_id, ProjectOwner::Pipeline)
            .await
            .unwrap();
        let processing = Processing::default();
        let _workflow = processing.try_admit().unwrap();
        let editing = page_project(root.path(), "Editing").await;
        let editing_id = editing.instance_id;
        let page = editing.active_page().unwrap();
        projects.replace(editing).await;
        let revision = {
            let mut active = projects.lock().await;
            let active = active.as_mut().unwrap();
            let (paint, layer) = active
                .apply_raster_stroke(
                    page,
                    None,
                    RasterStrokeMode::Paint,
                    [0, 0, 0, 255],
                    12.0,
                    vec![ScenePoint { x: 32.0, y: 32.0 }],
                )
                .await
                .unwrap();
            active.record_commit(&paint);
            let (erase, _) = active
                .apply_raster_stroke(
                    page,
                    Some(layer),
                    RasterStrokeMode::Erase,
                    [0; 4],
                    12.0,
                    vec![ScenePoint { x: 32.0, y: 32.0 }],
                )
                .await
                .unwrap();
            active.record_commit(&erase);
            let snapshot = active.snapshot();
            let blob = snapshot
                .asset(layer, &AssetRole::new("source").unwrap())
                .unwrap()
                .unwrap()
                .blob;
            let image = image::load_from_memory(&snapshot.read_blob(blob).await.unwrap())
                .unwrap()
                .to_rgba8();
            assert_eq!(image.get_pixel(32, 32).0[3], 0);
            active.undo().await.unwrap();
            let snapshot = active.snapshot();
            let blob = snapshot
                .asset(layer, &AssetRole::new("source").unwrap())
                .unwrap()
                .unwrap()
                .blob;
            let image = image::load_from_memory(&snapshot.read_blob(blob).await.unwrap())
                .unwrap()
                .to_rgba8();
            assert_eq!(
                image.get_pixel(32, 32).0[3],
                255,
                "eraser Undo must remain local to the edited project"
            );
            active.revision()
        };
        let (target, request) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            prepare_inpaint(&projects, revision, vec![Point { x: 16.0, y: 16.0 }], 8.0),
        )
        .await
        .expect("Remove preparation must not wait for the background workflow")
        .unwrap();
        assert_eq!(target.id, editing_id);
        assert!(
            processing.try_admit().is_err(),
            "Remove must not take or release the background workflow reservation"
        );
        projects
            .replace(page_project(root.path(), "Third").await)
            .await;
        let mask = request.inpainting_mask.unwrap();
        assert_eq!(mask.page, page);
        assert_eq!(
            image::load_from_memory(&mask.png)
                .unwrap()
                .to_luma8()
                .get_pixel(16, 16)
                .0[0],
            255
        );
        let snapshot = projects.snapshot(&target).await.unwrap();
        assert!(snapshot.page(page).is_ok());
        assert_eq!(projects.target(&target).await.unwrap().0.name, "Editing");
        assert_eq!(
            projects.snapshot(&background).await.unwrap().revision(),
            background_revision
        );
        projects.finish(&target).await;
        assert!(projects.target(&background).await.is_ok());
        projects.finish(&background).await;
    }

    #[tokio::test]
    async fn rejected_remove_releases_its_project_and_stale_canvas_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let projects = ProjectWorkspace::default();
        let editing = page_project(root.path(), "Editing").await;
        let revision = editing.revision();
        projects.replace(editing).await;
        assert!(
            prepare_inpaint(
                &projects,
                revision,
                vec![Point {
                    x: -100.0,
                    y: -100.0
                }],
                8.0
            )
            .await
            .is_err()
        );
        let (target, _) =
            prepare_inpaint(&projects, revision, vec![Point { x: 16.0, y: 16.0 }], 8.0)
                .await
                .expect("failed mask encoding must not leak the project lease");
        projects.finish(&target).await;
        {
            let mut project = projects.lock().await;
            project
                .as_mut()
                .unwrap()
                .set_wiki("characters".into(), "changed".into())
                .await
                .unwrap();
        }
        let error = prepare_inpaint(&projects, revision, vec![Point { x: 16.0, y: 16.0 }], 8.0)
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("canvas edit expected revision"));
    }
}
