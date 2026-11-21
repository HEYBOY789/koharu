//! Inspects rendering at reader widths and applies bounded lettering edits.

use std::collections::BTreeSet;

use anyhow::Result;
use image::RgbaImage;
use koharu_renderer::{Renderer, ResolvedText};
use koharu_scene::{
    Anchor, Bounds, Content, Edit, Fit, Frame, LayerId, Op, PageId, PathFit, Project, Rgba,
    Session, Text, Transform,
};
use kurbo::{Affine, Rect, Shape as _};
use rig_core::{
    completion::ToolAnnotations,
    tool::{Tool, ToolContext, ToolExecutionError},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::tools::{failed, project, schema};

const PHONE: f64 = 390.0;
const DESKTOP: f64 = 960.0;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Issue {
    Small,
    Overflow,
    MissingGlyphs,
    ThickStroke,
    Contrast,
    Overlap,
    Duplicate,
}

#[derive(Serialize)]
pub(crate) struct Finding {
    issue: Issue,
    layers: Vec<LayerId>,
    mobile_px: f64,
    desktop_px: f64,
}

#[derive(Serialize)]
pub(crate) struct Report {
    page: PageId,
    checked: usize,
    edited: usize,
    findings: Vec<Finding>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuditArgs {
    /// Page ID from get_project.
    page: PageId,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RepairArgs {
    /// Page ID from get_project.
    page: PageId,
}

pub(crate) struct AuditReadability;
pub(crate) struct RepairReadability;
pub(crate) struct VerifyReadability;

impl Tool for AuditReadability {
    const NAME: &'static str = "audit_readability";
    type Args = AuditArgs;
    type Output = Report;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Measure rendered lettering at 390 and 960 px page widths. Report small text, overflow, missing glyphs, thick strokes, low contrast and overlaps by layer ID.".into()
    }

    fn parameters(&self) -> Value {
        schema::<AuditArgs>()
    }

    fn annotations(&self) -> ToolAnnotations {
        ToolAnnotations::default().with_read_only(true)
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: AuditArgs,
    ) -> Result<Report, Self::Error> {
        inspect(context, args.page, false).await
    }
}

impl Tool for VerifyReadability {
    const NAME: &'static str = "verify_readability";
    type Args = AuditArgs;
    type Output = Report;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Recheck the current rendering of a page after editing. Give its page ID. The report lists only current findings.".into()
    }

    fn parameters(&self) -> Value {
        schema::<AuditArgs>()
    }

    fn annotations(&self) -> ToolAnnotations {
        ToolAnnotations::default().with_read_only(true)
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: AuditArgs,
    ) -> Result<Report, Self::Error> {
        inspect(context, args.page, false).await
    }
}

impl Tool for RepairReadability {
    const NAME: &'static str = "repair_readability";
    type Args = RepairArgs;
    type Output = Report;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Repair lettering on one inspected page. Keep the wording and ink colors. Apply fitting, spacing and border changes only if the proposed rendered layout improves. Hide exact overlapping duplicates while retaining one visible copy. Return the remaining findings.".into()
    }

    fn parameters(&self) -> Value {
        schema::<RepairArgs>()
    }

    async fn call(
        &self,
        context: &mut ToolContext,
        args: RepairArgs,
    ) -> Result<Report, Self::Error> {
        inspect(context, args.page, true).await
    }
}

async fn inspect(
    context: &ToolContext,
    page: PageId,
    repair: bool,
) -> Result<Report, ToolExecutionError> {
    let tools = project(context)?;
    if repair {
        tools.check_page(page)?;
    }
    tokio::task::spawn_blocking(move || {
        let mut session = tools.session.lock();
        let inspection = Inspection::read(&tools.renderer, &session, page).map_err(failed)?;
        if !repair {
            return Ok(inspection.report(page, 0));
        }
        let count = inspection
            .repair(&tools.renderer, &mut session)
            .map_err(failed)?;
        if count == 0 {
            Ok(inspection.report(page, 0))
        } else {
            Inspection::read(&tools.renderer, &session, page)
                .map(|after| after.report(page, count))
                .map_err(failed)
        }
    })
    .await
    .map_err(failed)?
}

#[derive(Clone)]
struct Sample {
    id: LayerId,
    text: Text,
    source: Option<String>,
    bounds: Rect,
    size: f64,
    stroke: f64,
    fill: Rgba,
    outline: Option<Rgba>,
    overflow: bool,
    missing: usize,
}

impl Sample {
    fn measured(
        id: LayerId,
        text: &Text,
        source: Option<String>,
        bounds: Rect,
        layout: &ResolvedText,
    ) -> Option<Self> {
        let run = layout
            .runs
            .iter()
            .min_by(|a, b| a.size.total_cmp(&b.size))?;
        let transform = Affine::from(text.transform);
        let scale = transform.determinant().abs().sqrt();
        Some(Self {
            id,
            text: text.clone(),
            source,
            bounds,
            size: f64::from(run.size) * scale,
            stroke: layout
                .runs
                .iter()
                .filter_map(|span| span.stroke.map(|(_, width)| f64::from(width) * scale))
                .fold(0.0, f64::max),
            fill: run.fill.into(),
            outline: run.stroke.map(|(color, _)| color.into()),
            overflow: layout.overflowed,
            missing: layout.uncovered.len(),
        })
    }
}

struct Inspection {
    page_bounds: Rect,
    background: RgbaImage,
    texts: Vec<Sample>,
}

impl Inspection {
    fn read(renderer: &Renderer, session: &Session, page: PageId) -> Result<Self> {
        let frame = renderer.render(session, page)?;
        let project = session.project();
        let mut texts = Vec::new();
        for layer in project.layers_of(page)? {
            let (text, source, drawing) = match &layer.content {
                Content::Text(text) => (text.as_ref(), None, frame.layer(layer.id)),
                Content::Block(block) => {
                    let Some(text) = &block.translation else {
                        continue;
                    };
                    (
                        text,
                        Some(block.source.content.text()),
                        frame.translation(layer.id),
                    )
                }
                _ => continue,
            };
            if let Some(drawing) = drawing.filter(|drawing| drawing.visible())
                && let Some(bounds) = drawing.bounds()
                && let Some(layout) = drawing.text()
                && let Some(sample) = Sample::measured(layer.id, text, source, bounds, layout)
            {
                texts.push(sample);
            }
        }
        let removed = texts.iter().map(|sample| sample.id).collect::<Vec<_>>();
        let background = frame.without(&removed).rasterize()?.image;
        Ok(Self {
            page_bounds: frame.area(),
            background,
            texts,
        })
    }

    fn issues(&self, sample: &Sample) -> Vec<Issue> {
        let mut issues = Vec::new();
        if sample.size * PHONE / self.page_bounds.width() < 9.5
            || sample.size * DESKTOP / self.page_bounds.width() < 13.0
        {
            issues.push(Issue::Small);
        }
        if sample.overflow || !contains(self.page_bounds, sample.bounds) {
            issues.push(Issue::Overflow);
        }
        if sample.missing != 0 {
            issues.push(Issue::MissingGlyphs);
        }
        if sample.stroke > sample.size * 0.11 {
            issues.push(Issue::ThickStroke);
        }
        if self.low_contrast(sample) > 0.25 {
            issues.push(Issue::Contrast);
        }
        issues
    }

    fn report(&self, page: PageId, edited: usize) -> Report {
        let mut findings = Vec::new();
        for (index, text) in self.texts.iter().enumerate() {
            let finding = |issue, layers| Finding {
                issue,
                layers,
                mobile_px: text.size * PHONE / self.page_bounds.width(),
                desktop_px: text.size * DESKTOP / self.page_bounds.width(),
            };
            findings.extend(
                self.issues(text)
                    .into_iter()
                    .map(|issue| finding(issue, vec![text.id])),
            );
            for other in &self.texts[index + 1..] {
                if overlap(text.bounds, other.bounds) > 0.18 {
                    let issue = if duplicate(text, other) {
                        Issue::Duplicate
                    } else {
                        Issue::Overlap
                    };
                    findings.push(finding(issue, vec![text.id, other.id]));
                }
            }
        }
        Report {
            page,
            checked: self.texts.len(),
            edited,
            findings,
        }
    }

    fn low_contrast(&self, text: &Sample) -> f64 {
        let bounds = text.bounds.intersect(self.page_bounds);
        if bounds.area() <= 0.0 {
            return 1.0;
        }
        let fill = luminance(text.fill.0);
        let outline = text
            .outline
            .filter(|_| text.stroke >= 0.65)
            .map(|color| luminance(color.0));
        let halo = outline.is_some_and(|halo| contrast(fill, halo) >= 4.0);
        let mut poor = 0;
        for row in 0..7 {
            for column in 0..7 {
                let x = bounds.x0 + bounds.width() * (f64::from(column) + 0.5) / 7.0;
                let y = bounds.y0 + bounds.height() * (f64::from(row) + 0.5) / 7.0;
                let Some(pixel) = self.background.get_pixel_checked(x as u32, y as u32) else {
                    continue;
                };
                let [r, g, b, alpha] = pixel.0.map(|channel| f32::from(channel) / 255.0);
                let background = luminance([
                    r * alpha + 1.0 - alpha,
                    g * alpha + 1.0 - alpha,
                    b * alpha + 1.0 - alpha,
                    1.0,
                ]);
                if contrast(fill, background) < 3.5 && !halo {
                    poor += 1;
                }
            }
        }
        f64::from(poor) / 49.0
    }

    fn score(&self, text: &Sample, others: &[Sample], hidden: &BTreeSet<LayerId>) -> f64 {
        let phone = text.size * PHONE / self.page_bounds.width();
        let overlap = others
            .iter()
            .filter(|other| other.id != text.id && !hidden.contains(&other.id))
            .map(|other| overlap(text.bounds, other.bounds))
            .filter(|area| *area > 0.12)
            .sum::<f64>();
        (9.5 - phone).max(0.0)
            + if text.overflow { 8.0 } else { 0.0 }
            + 8.0 * self.low_contrast(text)
            + 10.0 * overlap
            + 16.0 * (text.stroke / text.size.max(1.0) - 0.11).max(0.0)
            + text.missing as f64 * 10.0
    }

    fn repair(&self, renderer: &Renderer, session: &mut Session) -> Result<usize> {
        let project = session.project();
        let mut others = self.texts.clone();
        let mut hidden = BTreeSet::new();
        let mut ops = Vec::new();
        let mut changed = 0;
        for index in 0..others.len() {
            let current = others[index].clone();
            if hidden.contains(&current.id) || project.locked_at(current.id).is_some() {
                continue;
            }
            let owns_cleanup = matches!(&project.layer(current.id)?.content,
                Content::Block(block) if block.cleanup.visible && project.has_pixels(block.cleanup.id));
            if !owns_cleanup
                && others[..index].iter().any(|earlier| {
                    !hidden.contains(&earlier.id)
                        && duplicate(&current, earlier)
                        && overlap(current.bounds, earlier.bounds) > 0.48
                })
            {
                ops.push(Op::UpdateLayer {
                    layer: current.id.into(),
                    patch: json!({"visible": false}),
                });
                hidden.insert(current.id);
                changed += 1;
                continue;
            }
            let original = self.score(&current, &others, &hidden);
            if original <= 0.01 {
                continue;
            }
            let (mut best_score, mut best) = (original, None);
            let mut candidates = Vec::new();
            let border = self
                .issues(&current)
                .iter()
                .any(|issue| matches!(issue, Issue::Contrast | Issue::ThickStroke));
            if border {
                let mut text = current.text.clone();
                let color = current.outline.unwrap_or_else(|| {
                    let value = if luminance(current.fill.0) > 0.5 {
                        0.0
                    } else {
                        1.0
                    };
                    Rgba([value, value, value, 1.0])
                });
                let width = (current.size * 0.075).clamp(0.75, 3.5);
                text.style.chars.stroke = Some(color);
                text.style.chars.stroke_width = Some(width);
                for span in &mut text.content.0 {
                    if span.attributes.stroke_width.is_some() {
                        span.attributes.stroke_width = Some(width);
                    }
                }
                candidates.push(text);
            }
            let mut fitted = candidates
                .last()
                .cloned()
                .unwrap_or_else(|| current.text.clone());
            match &mut fitted.frame {
                Frame::Box { fit, .. } => *fit = Fit::Best,
                Frame::Path { fit, .. } => *fit = PathFit::Best,
                Frame::Point => {}
            }
            candidates.push(fitted.clone());
            let room = balloon_bounds(&project, current.id)?;
            if let Some(room) = room {
                let bounds = current.bounds;
                if let Frame::Box { bounds: old, .. } = &fitted.frame {
                    let mut spread = fitted.clone();
                    spread.frame = Frame::Box {
                        bounds: Bounds {
                            x: old.x - old.width * 0.14,
                            y: old.y - old.height * 0.10,
                            width: old.width * 1.28,
                            height: old.height * 1.20,
                        },
                        fit: Fit::Best,
                        anchor: Anchor::Center,
                    };
                    candidates.push(spread);
                }
                for shift in [-0.20, 0.20, -0.42, 0.42] {
                    let mut moved = fitted.clone();
                    let distance = bounds.height().max(current.size) * shift;
                    moved.transform = Transform::from(
                        Affine::translate((0.0, distance)) * Affine::from(moved.transform),
                    );
                    candidates.push(moved);
                }
                candidates.retain(|text| {
                    text.bounds().is_none_or(|frame| contains(room, frame))
                        || text.frame == current.text.frame
                });
            }
            for text in candidates {
                let Some(layout) = renderer.layout(current.id, &text)? else {
                    continue;
                };
                let bounds = Affine::from(text.transform)
                    .transform_rect_bbox(renderer.lettering(current.id, &text)?.bounds());
                let Some(candidate) =
                    Sample::measured(current.id, &text, current.source.clone(), bounds, &layout)
                else {
                    continue;
                };
                if candidate.size < current.size * 0.94
                    || candidate.missing > current.missing
                    || (!current.overflow && candidate.overflow)
                    || !contains(self.page_bounds, bounds)
                    || room.is_some_and(|bubble| {
                        !contains(bubble, bounds)
                            && (text.frame != current.text.frame
                                || text.transform != current.text.transform)
                    })
                {
                    continue;
                }
                if others.iter().any(|other| {
                    other.id != current.id
                        && !hidden.contains(&other.id)
                        && overlap(candidate.bounds, other.bounds)
                            > overlap(current.bounds, other.bounds).max(0.12) + 0.015
                }) {
                    continue;
                }
                let score = self.score(&candidate, &others, &hidden);
                if score < best_score - 0.075 {
                    best_score = score;
                    best = Some(candidate);
                }
            }
            if let Some(candidate) = best {
                let text = &candidate.text;
                let mut frame = serde_json::to_value(&text.frame)?;
                if let (Value::Object(old), Value::Object(new)) =
                    (serde_json::to_value(&current.text.frame)?, &mut frame)
                {
                    for (key, _) in old {
                        new.entry(key).or_insert(Value::Null);
                    }
                }
                let patch =
                    json!({ "style": text.style, "frame": frame, "transform": text.transform });
                ops.push(Op::UpdateLayer {
                    layer: current.id.into(),
                    patch: if matches!(project.layer(current.id)?.content, Content::Block(_)) {
                        json!({ "translation": patch })
                    } else {
                        patch
                    },
                });
                if text.content != current.text.content {
                    ops.push(Op::SetRichText {
                        layer: current.id.into(),
                        spans: text.content.0.clone(),
                    });
                }
                others[index] = candidate;
                changed += 1;
            }
        }
        if changed != 0 {
            session.apply(Edit {
                label: "readability.repair".into(),
                ops,
            })?;
        }
        Ok(changed)
    }
}

fn balloon_bounds(project: &Project, layer: LayerId) -> Result<Option<Rect>> {
    let (_, parent) = project.parent(layer)?;
    let parent = parent
        .filter(|layer| matches!(layer.content, Content::Balloon(_)))
        .or_else(|| {
            let layer = project.layer(layer).ok()?;
            let id: LayerId = serde_json::from_value(
                layer
                    .attributes
                    .get("koharu")?
                    .get("legacy_balloon")?
                    .clone(),
            )
            .ok()?;
            project.layer(id).ok()
        });
    match parent.map(|layer| &layer.content) {
        Some(Content::Balloon(balloon)) => {
            Ok(Some(kurbo::BezPath::from(&balloon.outline).bounding_box()))
        }
        _ => Ok(None),
    }
}

fn duplicate(a: &Sample, b: &Sample) -> bool {
    fn normalize(text: &str) -> String {
        text.chars()
            .filter(|ch| ch.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    }
    let first = normalize(&a.text.content.text());
    first.chars().count() >= 8
        && first == normalize(&b.text.content.text())
        && a.source
            .as_ref()
            .zip(b.source.as_ref())
            .is_some_and(|(a, b)| !a.is_empty() && normalize(a) == normalize(b))
}

fn overlap(a: Rect, b: Rect) -> f64 {
    a.intersect(b).area() / a.area().min(b.area()).max(1.0)
}

fn contains(outer: Rect, inner: Rect) -> bool {
    outer.x0 - 0.5 <= inner.x0
        && outer.y0 - 0.5 <= inner.y0
        && outer.x1 + 0.5 >= inner.x1
        && outer.y1 + 0.5 >= inner.y1
}

fn luminance([r, g, b, _]: [f32; 4]) -> f64 {
    let linear = |value: f32| {
        let value = f64::from(value);
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

fn contrast(a: f64, b: f64) -> f64 {
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}
