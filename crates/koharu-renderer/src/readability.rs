//! Device-sized review of the same prepared text used by canvas and exports.
use std::collections::HashSet;

use koharu_scene::EntityId;
use serde::Serialize;

use crate::{Frame, LayerKind, RenderBounds, RenderDiagnostic, frame::NodeDescriptor};

pub const READER_WIDTHS: [u32; 2] = [390, 960];
pub const MINIMUM_READER_FONT: f32 = 12.0;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReadabilityReport {
    pub page: EntityId,
    pub page_width: u32,
    pub page_height: u32,
    pub reader_widths: Vec<u32>,
    pub minimum_font_px: f32,
    pub layers: Vec<ReadableText>,
    pub issues: Vec<ReadabilityIssue>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReadableText {
    pub element: EntityId,
    pub text: String,
    pub source_font_px: f32,
    pub display_font_px: Vec<f32>,
    pub fonts: Vec<String>,
    pub bounds: [f32; 4],
    pub layout_bounds: [f32; 4],
    pub color: [u8; 4],
    pub opacity: f32,
    pub stroke_width: f32,
    pub stroke_color: Option<[u8; 4]>,
    pub background: Option<Background>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Background {
    pub dark_fraction: f32,
    pub light_fraction: f32,
    pub luminance_spread: f32,
    pub fill_contrast: f32,
    pub outline_contrast: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadabilityKind {
    SmallText,
    Overflow,
    ThickOutline,
    OverlapCandidate,
    DuplicateCandidate,
    LowContrast,
    OutsidePage,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ReadabilityIssue {
    pub id: String,
    pub element: EntityId,
    pub related: Option<EntityId>,
    pub kind: ReadabilityKind,
    /// Deficit in display pixels, outline/font ratio, or intersected area ratio.
    pub value: f32,
}

impl ReadabilityReport {
    /// Sample artwork + cleanup, with translated text excluded. Sampling the final
    /// outline would mistake the text's own white border for a white balloon.
    pub fn sample_background(&mut self, background: &image::RgbaImage) {
        self.issues
            .retain(|issue| issue.kind != ReadabilityKind::LowContrast);
        for layer in &mut self.layers {
            let [x, y, w, h] = layer.bounds;
            let mut samples = Vec::with_capacity(144);
            for row in 0..12 {
                for col in 0..12 {
                    let px = x + w * (col as f32 + 0.5) / 12.0;
                    let py = y + h * (row as f32 + 0.5) / 12.0;
                    if px >= 0.0
                        && py >= 0.0
                        && px < background.width() as f32
                        && py < background.height() as f32
                    {
                        let pixel = background.get_pixel(px as u32, py as u32).0;
                        // Transparent page areas are presented against white in exports.
                        let alpha = pixel[3] as f32 / 255.0;
                        let rgb = [0, 1, 2]
                            .map(|i| (pixel[i] as f32 * alpha + 255.0 * (1.0 - alpha)) as u8);
                        samples.push(luminance([rgb[0], rgb[1], rgb[2], 255]));
                    }
                }
            }
            if samples.is_empty() {
                continue;
            }
            samples.sort_by(f32::total_cmp);
            let n = samples.len();
            let fill = luminance(layer.color);
            let stroke = layer.stroke_color.map(luminance);
            let fill_contrast = percentile_contrast(
                &samples,
                fill,
                layer.opacity * layer.color[3] as f32 / 255.0,
            );
            let outline_contrast = stroke.map_or(0.0, |v| {
                percentile_contrast(
                    &samples,
                    v,
                    layer.opacity * layer.stroke_color.unwrap()[3] as f32 / 255.0,
                )
            });
            let spread = samples[n * 9 / 10] - samples[n / 10];
            layer.background = Some(Background {
                dark_fraction: samples.iter().filter(|v| **v < 0.1).count() as f32 / n as f32,
                light_fraction: samples.iter().filter(|v| **v > 0.8).count() as f32 / n as f32,
                luminance_spread: spread,
                fill_contrast,
                outline_contrast,
            });
            // Mixed artwork needs visual judgement. Contrast here is a conservative
            // flag for nearly uniform backgrounds, not a claim of WCAG compliance.
            let contrast = fill_contrast.max(outline_contrast);
            if spread < 0.1 && contrast < 4.5 {
                self.issues.push(issue(
                    layer.element,
                    None,
                    ReadabilityKind::LowContrast,
                    4.5 - contrast,
                ));
            }
        }
    }

    /// Reject measurable regressions, never infer a collision from rectangles alone.
    /// Candidates still require visual review, including intentionally kept styles.
    pub fn regressions(&self, after: &Self) -> Vec<String> {
        let mut result = Vec::new();
        for before in &self.layers {
            let Some(next) = after.layers.iter().find(|v| v.element == before.element) else {
                result.push(format!(
                    "{}: visible translated text disappeared",
                    before.element
                ));
                continue;
            };
            let old_size = before
                .display_font_px
                .iter()
                .copied()
                .fold(f32::INFINITY, f32::min);
            let new_size = next
                .display_font_px
                .iter()
                .copied()
                .fold(f32::INFINITY, f32::min);
            if new_size < self.minimum_font_px && new_size + 0.25 < old_size {
                result.push(format!(
                    "{}: smallest display font shrank from {old_size:.1} to {new_size:.1}px",
                    before.element
                ));
            }
        }
        for next in &after.issues {
            if !matches!(
                next.kind,
                ReadabilityKind::Overflow
                    | ReadabilityKind::ThickOutline
                    | ReadabilityKind::LowContrast
                    | ReadabilityKind::OutsidePage
            ) {
                continue;
            }
            let old = self
                .issues
                .iter()
                .find(|v| v.id == next.id)
                .map_or(0.0, |v| v.value);
            if next.value > old + 0.01 {
                result.push(format!(
                    "{}: {:?} worsened ({old:.2} → {:.2})",
                    next.element, next.kind, next.value
                ));
            }
        }
        result
    }
}

impl Frame {
    pub fn readability(&self) -> ReadabilityReport {
        let (width, height) = self.size();
        let layers = self
            .layers()
            .iter()
            .filter_map(|layer| {
                let LayerKind::Text(text) = layer.kind() else {
                    return None;
                };
                if !layer.presentation().visible
                    || layer.presentation().opacity <= 0.0
                    || text.text.trim().is_empty()
                {
                    return None;
                }
                let stroke = match &layer.0.node.descriptor {
                    NodeDescriptor::Text(descriptor) => descriptor
                        .stroke
                        .filter(|s| s.color[3] > 0 && s.width_px > 0.0),
                    _ => None,
                };
                if text.color[3] == 0 && stroke.is_none() {
                    return None;
                }
                Some(ReadableText {
                    element: layer.entity(),
                    text: text.text.clone(),
                    source_font_px: text.font_size,
                    display_font_px: READER_WIDTHS
                        .iter()
                        .map(|w| text.font_size * (*w as f32 / width.max(1) as f32).min(1.0))
                        .collect(),
                    fonts: text.post_script_fonts.clone(),
                    bounds: bounds(layer.bounds()),
                    layout_bounds: bounds(text.layout_bounds),
                    color: text.color,
                    opacity: layer.presentation().opacity,
                    stroke_width: stroke.map_or(0.0, |s| s.width_px),
                    stroke_color: stroke.map(|s| s.color),
                    background: None,
                })
            })
            .collect::<Vec<_>>();
        let visible = layers.iter().map(|v| v.element).collect::<HashSet<_>>();
        let mut issues = Vec::new();
        for layer in &layers {
            let [left, top, w, h] = layer.bounds;
            let outside = (-left)
                .max(0.0)
                .max((-top).max(0.0))
                .max((left + w - width as f32).max(0.0))
                .max((top + h - height as f32).max(0.0));
            if outside > 1.0 {
                issues.push(issue(
                    layer.element,
                    None,
                    ReadabilityKind::OutsidePage,
                    outside,
                ));
            }
            let size = layer
                .display_font_px
                .iter()
                .copied()
                .fold(f32::INFINITY, f32::min);
            if size < MINIMUM_READER_FONT {
                issues.push(issue(
                    layer.element,
                    None,
                    ReadabilityKind::SmallText,
                    MINIMUM_READER_FONT - size,
                ));
            }
            let ratio = layer.stroke_width / layer.source_font_px.max(1.0);
            if ratio > 0.08 {
                issues.push(issue(
                    layer.element,
                    None,
                    ReadabilityKind::ThickOutline,
                    ratio,
                ));
            }
        }
        for diagnostic in self.diagnostics() {
            if let RenderDiagnostic::TextOverflow {
                entity,
                available,
                actual_width,
                actual_height,
                ..
            } = diagnostic
            {
                if visible.contains(entity) {
                    let amount = (actual_width / available.width.max(1.0))
                        .max(actual_height / available.height.max(1.0));
                    issues.push(issue(*entity, None, ReadabilityKind::Overflow, amount));
                }
            }
        }
        for (i, left) in layers.iter().enumerate() {
            for right in &layers[i + 1..] {
                let ratio = intersection(left.bounds, right.bounds);
                if ratio > 0.05 {
                    let duplicate = ratio > 0.7 && normalize(&left.text) == normalize(&right.text);
                    issues.push(issue(
                        left.element,
                        Some(right.element),
                        if duplicate {
                            ReadabilityKind::DuplicateCandidate
                        } else {
                            ReadabilityKind::OverlapCandidate
                        },
                        ratio,
                    ));
                }
            }
        }
        ReadabilityReport {
            page: self.page(),
            page_width: width,
            page_height: height,
            reader_widths: READER_WIDTHS.to_vec(),
            minimum_font_px: MINIMUM_READER_FONT,
            layers,
            issues,
        }
    }

    /// Keep all visible raster content, including cleanup, at its authored opacity.
    pub fn without_text(&self) -> crate::Result<Self> {
        self.filtered_layers(|layer| !matches!(layer.kind(), LayerKind::Text(_)))
    }

    /// Show source artwork before translated lettering, cleanup and paint.
    pub fn original_artwork(&self) -> crate::Result<Self> {
        self.filtered_layers(|layer| matches!(layer.kind(), LayerKind::Image(image) if image.kind == crate::ImageKind::Source))
    }

    fn filtered_layers(&self, keep: impl Fn(&crate::Layer) -> bool) -> crate::Result<Self> {
        let layers: std::sync::Arc<[crate::Layer]> = self
            .layers()
            .iter()
            .filter(|layer| keep(layer))
            .cloned()
            .collect::<Vec<_>>()
            .into();
        let prepared = crate::frame::prepare_frame(
            self.revision(),
            self.page(),
            self.0.width,
            self.0.height,
            self.0.origin,
            self.0.normalization,
            &layers,
        )?;
        Ok(Self(std::sync::Arc::new(crate::frame::FrameData {
            revision: self.revision(),
            page: self.page(),
            width: self.0.width,
            height: self.0.height,
            origin: self.0.origin,
            normalization: self.0.normalization,
            layer_index: std::sync::Arc::new(
                layers
                    .iter()
                    .enumerate()
                    .map(|(i, l)| (l.entity(), i))
                    .collect(),
            ),
            layers,
            dependencies: self.0.dependencies.clone(),
            diagnostics: self.0.diagnostics.clone(),
            stats: self.0.stats,
            prepared: std::sync::Arc::new(prepared),
        })))
    }
}

fn bounds(b: RenderBounds) -> [f32; 4] {
    [b.x, b.y, b.width, b.height]
}
fn normalize(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}
fn intersection(a: [f32; 4], b: [f32; 4]) -> f32 {
    let w = ((a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0])).max(0.0);
    let h = ((a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1])).max(0.0);
    w * h / (a[2] * a[3]).min(b[2] * b[3]).max(1.0)
}
fn issue(
    element: EntityId,
    related: Option<EntityId>,
    kind: ReadabilityKind,
    value: f32,
) -> ReadabilityIssue {
    ReadabilityIssue {
        id: format!(
            "{kind:?}:{element}:{}",
            related.map_or(String::new(), |id| id.to_string())
        ),
        element,
        related,
        kind,
        value,
    }
}
fn luminance(c: [u8; 4]) -> f32 {
    let linear = [0, 1, 2].map(|i| {
        let v = c[i] as f32 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    });
    linear[0] * 0.2126 + linear[1] * 0.7152 + linear[2] * 0.0722
}
fn percentile_contrast(samples: &[f32], value: f32, alpha: f32) -> f32 {
    let mut contrasts = samples
        .iter()
        .map(|v| {
            let composited = value * alpha + v * (1.0 - alpha);
            (v.max(composited) + 0.05) / (v.min(composited) + 0.05)
        })
        .collect::<Vec<_>>();
    contrasts.sort_by(f32::total_cmp);
    contrasts[contrasts.len() / 10]
}

#[cfg(test)]
mod tests {
    use super::*;
    use koharu_scene::{
        At, Authored, Geometry, Origin, PageDraft, Session, SourceText, TextLayout, TextLayoutKind,
        Translation, Typography, Visibility,
    };

    #[tokio::test]
    async fn actual_fitted_text_is_audited_at_reader_width_and_hidden_text_is_excluded() {
        let mut session = Session::memory().await.unwrap();
        let mut ids = vec![];
        let patch = session
            .snapshot()
            .patch(|edit| {
                let page = edit.add_page(PageDraft::new("001", 1000.0, 1400.0), At::End)?;
                ids.push(page);
                for n in 0..3 {
                    let content = edit.add_text_content(page, At::End)?;
                    edit.set(
                        content,
                        &SourceText {
                            text: Authored::user("Readable dialogue".into()),
                            language: None,
                        },
                    )?;
                    edit.set(
                        content,
                        &Translation {
                            text: Authored::user("Readable dialogue".into()),
                            language: None,
                        },
                    )?;
                    let text = edit.add_text_layer(
                        page,
                        At::End,
                        content,
                        &TextLayout {
                            origin: Origin::User,
                            kind: TextLayoutKind::Paragraph,
                            angle_degrees: None,
                        },
                    )?;
                    edit.set(text, &Geometry::rectangle(30.0, 40.0, 500.0, 150.0))?;
                    edit.set(
                        text,
                        &Typography {
                            origin: Origin::User,
                            preferred_font: None,
                            font_weight: None,
                            font_style: None,
                            size: Some(24.0),
                            auto_fit: false,
                            color: Some([0, 0, 0, 255]),
                            stroke_color: Some([255, 255, 255, 255]),
                            stroke_width: Some(5.0),
                            alignment: None,
                            writing_mode: None,
                            extensions: Default::default(),
                        },
                    )?;
                    if n == 2 {
                        edit.set(
                            text,
                            &Visibility {
                                origin: Origin::User,
                                visible: false,
                                opacity: 1.0,
                            },
                        )?;
                    }
                    ids.push(text);
                }
                Ok(())
            })
            .unwrap();
        let snapshot = session.commit(patch).await.unwrap().snapshot;
        let renderer = crate::Renderer::default();
        let frame = renderer.render(&snapshot, ids[0]).await.unwrap();
        let before = frame.readability();
        assert_eq!(before.layers.len(), 2);
        assert!((before.layers[0].display_font_px[0] - 9.36).abs() < 0.01);
        for kind in [
            ReadabilityKind::SmallText,
            ReadabilityKind::ThickOutline,
            ReadabilityKind::DuplicateCandidate,
        ] {
            assert!(before.issues.iter().any(|i| i.kind == kind), "{kind:?}");
        }
        assert!(frame.without_text().unwrap().layers().is_empty());
        let bad = snapshot
            .patch(|edit| {
                let mut style = snapshot.text_layer(ids[1])?.typography()?.unwrap();
                style.size = Some(12.0);
                style.stroke_width = Some(8.0);
                edit.set(ids[1], &style)
            })
            .unwrap();
        let candidate = snapshot.preview([&bad]).unwrap();
        let after = renderer
            .render(&candidate, ids[0])
            .await
            .unwrap()
            .readability();
        assert!(
            before
                .regressions(&after)
                .iter()
                .any(|v| v.contains("shrank"))
        );
        assert!(
            before
                .regressions(&after)
                .iter()
                .any(|v| v.contains("ThickOutline"))
        );
        assert_eq!(
            snapshot
                .text_layer(ids[1])
                .unwrap()
                .typography()
                .unwrap()
                .unwrap()
                .size,
            Some(24.0),
            "preview does not save the regression"
        );
        let fixed = snapshot
            .patch(|edit| {
                for id in &ids[1..3] {
                    let mut style = snapshot.text_layer(*id)?.typography()?.unwrap();
                    style.size = Some(36.0);
                    style.stroke_width = Some(0.0);
                    edit.set(*id, &style)?;
                }
                edit.set(ids[2], &Geometry::rectangle(30.0, 300.0, 500.0, 150.0))
            })
            .unwrap();
        let candidate = snapshot.preview([&fixed]).unwrap();
        let fixed = renderer
            .render(&candidate, ids[0])
            .await
            .unwrap()
            .readability();
        assert!(fixed.issues.is_empty(), "{:?}", fixed.issues);
        assert!(before.regressions(&fixed).is_empty());
        let outside = candidate
            .patch(|edit| edit.set(ids[1], &Geometry::rectangle(-400.0, 40.0, 500.0, 150.0)))
            .unwrap();
        let escaped = candidate.preview([&outside]).unwrap();
        let report = renderer
            .render(&escaped, ids[0])
            .await
            .unwrap()
            .readability();
        assert!(
            fixed
                .regressions(&report)
                .iter()
                .any(|v| v.contains("OutsidePage")),
            "moving readable text off the page must be rejected"
        );
    }

    #[test]
    fn background_sampling_distinguishes_light_dark_and_mixed_art_without_sampling_text() {
        let id = EntityId::new();
        let text = ReadableText {
            element: id,
            text: "test".into(),
            source_font_px: 36.0,
            display_font_px: vec![14.04, 34.56],
            fonts: vec![],
            bounds: [0.0, 0.0, 100.0, 100.0],
            layout_bounds: [0.0, 0.0, 100.0, 100.0],
            color: [255, 255, 255, 255],
            opacity: 1.0,
            stroke_width: 0.0,
            stroke_color: None,
            background: None,
        };
        let report = ReadabilityReport {
            page: EntityId::new(),
            page_width: 1000,
            page_height: 1400,
            reader_widths: READER_WIDTHS.to_vec(),
            minimum_font_px: 12.0,
            layers: vec![text],
            issues: vec![],
        };
        let mut light = report.clone();
        light.sample_background(&image::RgbaImage::from_pixel(
            100,
            100,
            image::Rgba([255, 255, 255, 255]),
        ));
        assert!(
            light
                .issues
                .iter()
                .any(|i| i.kind == ReadabilityKind::LowContrast)
        );
        assert_eq!(
            light.layers[0].background.as_ref().unwrap().light_fraction,
            1.0
        );
        let mut dark = report.clone();
        dark.sample_background(&image::RgbaImage::from_pixel(
            100,
            100,
            image::Rgba([0, 0, 0, 255]),
        ));
        assert!(dark.issues.is_empty());
        let mut faded = dark.clone();
        faded.layers[0].opacity = 0.02;
        faded.sample_background(&image::RgbaImage::from_pixel(
            100,
            100,
            image::Rgba([0, 0, 0, 255]),
        ));
        assert!(
            faded
                .issues
                .iter()
                .any(|i| i.kind == ReadabilityKind::LowContrast),
            "nearly transparent white text on black must not be treated as opaque white"
        );
        assert_eq!(
            dark.layers[0].background.as_ref().unwrap().dark_fraction,
            1.0
        );
        let mixed = image::RgbaImage::from_fn(100, 100, |x, _| {
            image::Rgba(if x < 50 {
                [0, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            })
        });
        let mut report = report;
        report.sample_background(&mixed);
        assert!(
            report.layers[0]
                .background
                .as_ref()
                .unwrap()
                .luminance_spread
                > 0.8
        );
        assert!(
            report.issues.is_empty(),
            "mixed artwork requires visual judgement"
        );
        let mut title = dark;
        title.layers[0].color = [160, 70, 140, 255];
        title.layers[0].stroke_color = Some([255, 255, 255, 255]);
        title.layers[0].stroke_width = 1.0;
        title.issues.clear();
        let black = image::RgbaImage::from_pixel(100, 100, image::Rgba([0, 0, 0, 255]));
        title.sample_background(&black);
        let mut stripped = title.clone();
        stripped.layers[0].stroke_color = None;
        stripped.layers[0].stroke_width = 0.0;
        stripped.issues.clear();
        stripped.sample_background(&black);
        assert!(
            !title.regressions(&stripped).is_empty(),
            "removing a bright outline must not leave a dim colored title on black"
        );
    }
}
