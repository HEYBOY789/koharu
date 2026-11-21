use anyhow::{Context, Result, bail, ensure};
use kurbo::{Affine, BezPath, Point, Rect, Vec2};
use legacy::{RegionSpec as _, TextDirection, TextLayoutKind, WritingMode};
use serde_json::json;

use super::{file, legacy, scene};

pub(super) struct PageLayers<'a> {
    snapshot: &'a legacy::Snapshot,
    source: &'a legacy::AssetRole,
    page: scene::PageId,
    width: u32,
    height: u32,
    ops: &'a mut Vec<scene::Op>,
    pub text_layers: usize,
}

impl<'a> PageLayers<'a> {
    pub fn new(
        snapshot: &'a legacy::Snapshot,
        source: &'a legacy::AssetRole,
        page: scene::PageId,
        width: u32,
        height: u32,
        ops: &'a mut Vec<scene::Op>,
    ) -> Self {
        Self {
            snapshot,
            source,
            page,
            width,
            height,
            ops,
            text_layers: 0,
        }
    }

    pub async fn children(
        &mut self,
        parent: legacy::EntityId,
        parent_layer: Option<scene::LayerId>,
    ) -> Result<()> {
        let snapshot = self.snapshot;
        let mut index = 0;
        for entity in snapshot.children(parent)? {
            let id = scene::LayerId(entity.as_uuid());
            let mut pixels = None;
            let mut attributes = scene::Attributes::new();
            let mut descendants = false;
            let (name, content) = if let Some(group) =
                snapshot.component::<legacy::Group>(entity)?
            {
                descendants = true;
                (group.name, scene::Content::Group(scene::Group {}))
            } else if let Some(raster) = snapshot.component::<legacy::RasterLayer>(entity)? {
                if let Some(asset) = snapshot.asset(entity, self.source)? {
                    let bytes = snapshot.read_blob(asset.blob).await?;
                    let image = image::load_from_memory(&bytes)?.into_rgba8();
                    let (width, height) = image.dimensions();
                    ensure!(
                        (width, height) == (self.width, self.height),
                        "raster {entity} has dimensions {width}×{height} outside the legacy page grid"
                    );
                    pixels = Some((
                        scene::Rect {
                            x: 0,
                            y: 0,
                            width,
                            height,
                        },
                        image.into_raw(),
                    ));
                }
                (
                    raster.name,
                    scene::Content::Raster(scene::Raster::default()),
                )
            } else if let Some(layout) = snapshot.component::<legacy::TextLayout>(entity)? {
                self.text_layers += 1;
                let layer = snapshot.text_layer(entity)?;
                let content = layer.content()?;
                let region = content.source_region()?;
                let layout_frame = layer.frame()?;
                let source_frame = region.map(|region| region.geometry()).transpose()?;
                let ocr = region.map(|region| region.ocr()).transpose()?.flatten();
                let source_direction = match ocr.as_ref().map(|ocr| ocr.direction) {
                    Some(TextDirection::Vertical) => scene::Direction::Vertical,
                    _ => scene::Direction::Horizontal,
                };
                let source = content.source()?;
                let translation = content.translation()?;
                let typography = layer.typography()?;
                let balloon = layer.balloon_target()?;
                let shared_balloon = match balloon {
                    Some(region) => {
                        snapshot
                            .relations_to_as::<legacy::FlowsIn>(region.id())
                            .take(2)
                            .count()
                            > 1
                    }
                    None => false,
                };
                let balloon_shape = (!shared_balloon)
                    .then_some(balloon)
                    .flatten()
                    .map(|region| region.geometry())
                    .transpose()?;
                let layout_frame = if shared_balloon
                    && snapshot.component::<legacy::Geometry>(entity)?.is_none()
                {
                    source_frame.as_ref().or(layout_frame.as_ref())
                } else {
                    layout_frame.as_ref()
                };
                let (frame, transform) = text_frame(
                    layout_frame,
                    &layout,
                    typography.as_ref(),
                    balloon_shape.as_ref(),
                )?;
                let (source_box, source_transform) =
                    text_frame(source_frame.as_ref().or(layout_frame), &layout, None, None)?;
                let source_lang = source.as_ref().and_then(|v| v.language.as_ref());
                let mut source_text = scene::Text {
                    content: source
                        .as_ref()
                        .map(|v| scene::RichText::from(v.text.value.as_str()))
                        .unwrap_or_default(),
                    frame: source_box,
                    direction: source_direction,
                    transform: source_transform,
                    ..Default::default()
                };
                if let Some(language) = source_lang {
                    source_text.language = language
                        .as_str()
                        .parse()
                        .context("invalid source language")?;
                }
                let translation = translation
                    .map(|v| -> Result<scene::Text> {
                        let mut translation = scene::Text {
                            content: scene::RichText::from(v.text.value.as_str()),
                            style: style(typography.as_ref()),
                            frame,
                            direction: if typography.as_ref().and_then(|t| t.writing_mode)
                                == Some(WritingMode::Vertical)
                            {
                                scene::Direction::Vertical
                            } else {
                                scene::Direction::Horizontal
                            },
                            transform,
                            ..Default::default()
                        };
                        if let Some(language) = v.language {
                            translation.language = language
                                .as_str()
                                .parse()
                                .context("invalid translation language")?;
                        }
                        Ok(translation)
                    })
                    .transpose()?;
                let role = content.role()?.map(|role| role.role);
                attributes.insert(
                    "koharu".into(),
                    [
                        ("legacy_content".into(), json!(content.id())),
                        (
                            "legacy_region".into(),
                            json!(region.map(|region| region.id())),
                        ),
                        (
                            "legacy_balloon".into(),
                            json!(balloon.map(|region| region.id())),
                        ),
                        ("legacy_role".into(), json!(role)),
                    ]
                    .into(),
                );
                (
                    role.clone().unwrap_or_default(),
                    block(
                        if role.as_deref() == Some("dev.koharu.text.sfx") {
                            scene::Role::Sfx
                        } else {
                            scene::Role::Text
                        },
                        source_text,
                        translation,
                    )?,
                )
            } else if let Some(region) = snapshot.component::<legacy::Region>(entity)? {
                let name = region.label.unwrap_or_default();
                let geometry = snapshot.component::<legacy::Geometry>(entity)?;
                let content = if region.kind == legacy::PanelRegion::kind() {
                    scene::Content::Panel(scene::Panel {
                        outline: outline(geometry.as_ref().context("missing panel geometry")?)?,
                    })
                } else if region.kind == legacy::BubbleRegion::kind() {
                    scene::Content::Balloon(scene::Balloon {
                        outline: outline(geometry.as_ref().context("missing balloon geometry")?)?,
                    })
                } else {
                    attributes.insert(
                        "koharu".into(),
                        [
                            ("legacy_region_kind".into(), json!(region.kind)),
                            (
                                "legacy_detection".into(),
                                json!(snapshot.component::<legacy::DetectionAnalysis>(entity)?),
                            ),
                            (
                                "legacy_ocr".into(),
                                json!(snapshot.component::<legacy::OcrAnalysis>(entity)?),
                            ),
                        ]
                        .into(),
                    );
                    scene::Content::Shape(scene::Shape {
                        path: outline(
                            geometry
                                .as_ref()
                                .context("missing detected region geometry")?,
                        )?,
                        fill_rule: scene::FillRule::NonZero,
                        fill: None,
                        stroke: None,
                    })
                };
                (name, content)
            } else if snapshot.component::<legacy::TextContent>(entity)?.is_some() {
                ensure!(
                    snapshot
                        .relations_to_as::<legacy::Presents>(entity)
                        .next()
                        .is_some(),
                    "text {entity} has no presentation layer"
                );
                continue;
            } else if let Some(asset) = snapshot.asset(entity, self.source)? {
                let geometry = snapshot
                    .component::<legacy::Geometry>(entity)?
                    .context("embedded image has no geometry")?;
                let (bounds, transform) = oriented_box(&geometry, None)?;
                let image = snapshot.read_blob(asset.blob).await?;
                let dimensions = image::load_from_memory(&image)?.into_rgba8().dimensions();
                let placement = kurbo::Affine::from(transform)
                    * Affine::translate((bounds.x, bounds.y))
                    * Affine::scale_non_uniform(
                        bounds.width / f64::from(dimensions.0),
                        bounds.height / f64::from(dimensions.1),
                    );
                (
                    String::new(),
                    scene::Content::Image(scene::Image {
                        file: file(snapshot, asset, self.ops).await?,
                        transform: placement.into(),
                    }),
                )
            } else {
                bail!("unsupported legacy scene entity {entity}");
            };
            let presentation = snapshot.component::<legacy::Visibility>(entity)?;
            self.ops.push(scene::Op::AddLayer {
                id: Some(id.into()),
                page: self.page.into(),
                parent: parent_layer.map(Into::into),
                index,
                layer: Box::new(scene::NewLayer {
                    name,
                    visible: presentation.as_ref().is_none_or(|v| v.visible),
                    locked: false,
                    opacity: scene::Opacity::try_from(presentation.map_or(1.0, |v| v.opacity))
                        .map_err(anyhow::Error::msg)?,
                    blend: None,
                    attributes,
                    content,
                }),
            });
            index += 1;
            if let Some((rect, pixels)) = pixels {
                self.ops.push(scene::Op::WritePixels {
                    layer: id.into(),
                    rect,
                    pixels,
                });
            }
            if descendants {
                Box::pin(self.children(entity, Some(id))).await?;
            } else {
                ensure!(
                    snapshot.children(entity)?.len() == 0,
                    "unexpected children of {entity}"
                );
            }
        }
        Ok(())
    }
}

fn block(
    role: scene::Role,
    source: scene::Text,
    translation: Option<scene::Text>,
) -> Result<scene::Content> {
    Ok(serde_json::from_value(json!({
        "kind": "block",
        "role": role,
        "source": source,
        "translation": translation,
    }))?)
}

fn text_frame(
    geometry: Option<&legacy::Geometry>,
    layout: &legacy::TextLayout,
    typography: Option<&legacy::Typography>,
    balloon: Option<&legacy::Geometry>,
) -> Result<(scene::Frame, scene::Transform)> {
    let Some(geometry) = geometry else {
        return Ok((scene::Frame::Point, scene::Transform::default()));
    };
    let (bounds, transform) = oriented_box(geometry, layout.angle_degrees)?;
    if let Some(shape) = balloon
        && geometry.points == shape.points
    {
        let path = outline(shape)?;
        return Ok((
            scene::Frame::Path {
                path,
                fit: if typography.is_some_and(|t| !t.auto_fit) {
                    scene::PathFit::None
                } else {
                    scene::PathFit::Best
                },
                anchor: scene::Anchor::Center,
                inset: scene::Frame::DEFAULT_INSET,
            },
            scene::Transform::default(),
        ));
    }
    if layout.kind == TextLayoutKind::Point {
        return Ok((
            scene::Frame::Point,
            (Affine::from(transform) * Affine::translate((bounds.x, bounds.y))).into(),
        ));
    }
    let fit = if typography.is_some_and(|t| !t.auto_fit) {
        scene::Fit::None
    } else {
        scene::Fit::Best
    };
    Ok((
        scene::Frame::Box {
            bounds,
            fit,
            anchor: scene::Anchor::Center,
        },
        transform,
    ))
}

fn outline(geometry: &legacy::Geometry) -> Result<scene::Path> {
    let mut points = geometry.points.iter();
    let first = points.next().context("empty legacy geometry")?;
    let mut path = BezPath::new();
    path.move_to((first.x, first.y));
    for point in points {
        path.line_to((point.x, point.y));
    }
    path.close_path();
    scene::Path::try_from(path.to_svg()).map_err(anyhow::Error::msg)
}

fn oriented_box(
    geometry: &legacy::Geometry,
    angle: Option<f32>,
) -> Result<(scene::Bounds, scene::Transform)> {
    let mut points = geometry
        .points
        .iter()
        .map(|point| Point::new(point.x, point.y));
    let first = points.next().context("empty legacy geometry")?;
    let direction = points.next().map_or(Vec2::ZERO, |point| point - first);
    let degrees = angle.map(f64::from).unwrap_or_else(|| {
        if geometry.points.len() == 4 {
            direction.atan2().to_degrees()
        } else {
            0.0
        }
    });
    let rotate = Affine::rotate(degrees.to_radians());
    let local = rotate.inverse();
    let bounds = geometry.points.iter().fold(
        Rect::from_points(local * first, local * first),
        |rect, point| rect.union_pt(local * Point::new(point.x, point.y)),
    );
    Ok((
        scene::Bounds {
            x: bounds.x0,
            y: bounds.y0,
            width: bounds.width(),
            height: bounds.height(),
        },
        rotate.into(),
    ))
}

fn style(typography: Option<&legacy::Typography>) -> scene::TextStyle {
    let mut style = scene::TextStyle {
        align: Some(scene::Align::Center),
        line_height: Some(1.2),
        ..Default::default()
    };
    if let Some(typography) = typography {
        let rgba =
            |components: [u8; 4]| scene::Rgba(components.map(|value| f32::from(value) / 255.0));
        let stroke = typography.stroke_width.filter(|width| *width > 0.0);
        style.chars = scene::CharStyle {
            family: typography.preferred_font.clone(),
            weight: typography.font_weight,
            italic: typography
                .font_style
                .map(|style| style != legacy::FontStyle::Normal),
            size: typography.size.map(f64::from),
            fill: typography.color.map(rgba),
            stroke: stroke.map(|_| rgba(typography.stroke_color.unwrap_or([255; 4]))),
            stroke_width: stroke.map(f64::from),
        };
        style.align = Some(match typography.alignment {
            Some(legacy::TextAlignment::Start) => scene::Align::Start,
            Some(legacy::TextAlignment::End) => scene::Align::End,
            Some(legacy::TextAlignment::Justify) => scene::Align::Justify,
            _ => scene::Align::Center,
        });
    }
    style
}
