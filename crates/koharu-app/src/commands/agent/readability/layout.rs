//! Bounded layout proposals; the scene is committed only after visual selection.
use std::sync::Arc;

use anyhow::{Context as _, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use koharu_agent::{Control, Invocation};
use koharu_renderer::{Frame, LayerKind, ReadabilityKind, ReadabilityReport};
use koharu_scene::{EntityId, Geometry, Patch, Snapshot};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{repair_style, source_duplicates, typography};
use crate::commands::{editing::TypographyUpdate, output, project::Project};

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(in crate::commands::agent) struct LayoutSearch {
    pub element: String,
    /// Permitted placement rectangle [x, y, width, height] in source-image pixels.
    /// Inspect the artwork first; keep the rectangle inside its balloon/panel.
    pub area: [f32; 4],
    /// Protected rectangles [x, y, width, height], e.g. eyes, faces or panel edges.
    /// Existing visible text is protected automatically. At most 16 rectangles.
    #[serde(default)]
    pub avoid: Vec<[f32; 4]>,
}

struct Candidate {
    id: String,
    patch: Patch,
    frame: Frame,
    report: ReadabilityReport,
    geometry: [f32; 4],
    angle_degrees: f32,
    measured: koharu_renderer::ReadableText,
    displacement: f32,
}

pub(in crate::commands::agent) struct LayoutProposals {
    page: EntityId,
    element: EntityId,
    area: [f32; 4],
    attempts: usize,
    candidates: Vec<Candidate>,
}

impl LayoutProposals {
    pub async fn invocation(
        &self,
        rasterizer: Arc<koharu_rasterizer::Rasterizer>,
    ) -> Result<Invocation> {
        let mut result = Invocation::read(json!({
            "page":self.page,"element":self.element,"area":self.area,"attempts":self.attempts,
            "candidates":self.candidates.iter().map(|c| json!({
                "candidate":c.id,"frame":c.geometry,"angle_degrees":c.angle_degrees,"bounds":c.measured.bounds,"frame_bounds":c.measured.layout_bounds,
                "source_font_px":c.measured.source_font_px,"display_font_px":c.measured.display_font_px,
                "color":c.measured.color,"stroke_color":c.measured.stroke_color,"stroke_width":c.measured.stroke_width,
                "remaining_issues":c.report.issues.iter().filter(|i|i.element==self.element || i.related==Some(self.element)).collect::<Vec<_>>()
            })).collect::<Vec<_>>(),
            "next":if self.candidates.is_empty() {
                "The bounded search found no improving layout in this permitted area. Inspect the original/crop, try a different safe area, or report the remaining defect as needs_review. No edit was saved."
            } else {
                "No edit was saved. Compare the candidate images at phone and desktop sizes, then call repair_text with page and candidate (the chosen ID). Re-audit and view the saved page before verification."
            }
        }))?;
        for c in &self.candidates {
            for width in [390, 960] {
                result = result.with_image(
                    format!("Candidate {}: full page at {width}px", c.id),
                    format!(
                        "data:image/webp;base64,{}",
                        STANDARD.encode(
                            output::frame_preview(rasterizer.clone(), &c.frame, None, Some(width))
                                .await?
                        )
                    ),
                );
            }
        }
        Ok(result)
    }

    pub fn choose(&self, snapshot: &Snapshot, page: EntityId, id: &str) -> Result<(Patch, Value)> {
        ensure!(page == self.page, "Candidate belongs to another page");
        let c = self.candidates.iter().find(|c| c.id == id).context(
            "Unknown layout candidate; use an ID from the latest repair_text layout preview",
        )?;
        ensure!(
            c.patch.project_id() == snapshot.project_id()
                && c.patch.base_revision() == snapshot.revision(),
            "Layout candidate is stale. The project changed; audit/view and preview layouts again before choosing."
        );
        c.patch.validate_on(snapshot)?;
        Ok((
            c.patch.clone(),
            json!({"page":page,"element":self.element,"candidate":id,
            "remaining_issues":c.report.issues,"verification_required":true}),
        ))
    }
}

fn valid_rect(r: [f32; 4]) -> bool {
    r.iter().all(|v| v.is_finite())
        && r[2] > 0.0
        && r[3] > 0.0
        && (r[0] + r[2]).is_finite()
        && (r[1] + r[3]).is_finite()
}
fn contains(outer: [f32; 4], inner: [f32; 4]) -> bool {
    inner[0] >= outer[0] - 0.01
        && inner[1] >= outer[1] - 0.01
        && inner[0] + inner[2] <= outer[0] + outer[2] + 0.01
        && inner[1] + inner[3] <= outer[1] + outer[3] + 0.01
}
fn intersects(a: [f32; 4], b: [f32; 4]) -> bool {
    a[0].max(b[0]) < (a[0] + a[2]).min(b[0] + b[2]) - 0.01
        && a[1].max(b[1]) < (a[1] + a[3]).min(b[1] + b[3]) - 0.01
}

fn frames(area: [f32; 4], avoid: &[[f32; 4]], original: [f32; 4]) -> Vec<[f32; 4]> {
    let mut free = vec![area];
    for obstacle in avoid {
        let mut next = vec![];
        for r in free {
            if !intersects(r, *obstacle) {
                next.push(r);
                continue;
            }
            let [x, y, w, h] = r;
            for cut in [
                [x, y, (obstacle[0] - x).clamp(0.0, w), h],
                [
                    (obstacle[0] + obstacle[2]).max(x),
                    y,
                    (x + w - obstacle[0] - obstacle[2]).clamp(0.0, w),
                    h,
                ],
                [x, y, w, (obstacle[1] - y).clamp(0.0, h)],
                [
                    x,
                    (obstacle[1] + obstacle[3]).max(y),
                    w,
                    (y + h - obstacle[1] - obstacle[3]).clamp(0.0, h),
                ],
            ] {
                if cut[2] >= 12.0 && cut[3] >= 12.0 && !next.contains(&cut) {
                    next.push(cut);
                }
            }
        }
        next.sort_by(|a, b| (b[2] * b[3]).total_cmp(&(a[2] * a[3])));
        next.truncate(32);
        free = next;
    }
    let mut result = vec![];
    // Interleave aspect ratios across the largest free regions so the budget
    // cannot be consumed by small translations of a single rectangle.
    for (wf, hf) in [
        (1.0, 1.0),
        (0.82, 1.0),
        (1.0, 0.72),
        (0.65, 1.0),
        (0.82, 0.72),
    ] {
        for r in free.iter().take(4) {
            let w = r[2] * wf;
            let h = r[3] * hf;
            if w < 12.0 || h < 12.0 {
                continue;
            }
            let centered = [r[0] + (r[2] - w) * 0.5, r[1] + (r[3] - h) * 0.5, w, h];
            let anchored = [
                (original[0] + original[2] * 0.5 - w * 0.5).clamp(r[0], r[0] + r[2] - w),
                (original[1] + original[3] * 0.5 - h * 0.5).clamp(r[1], r[1] + r[3] - h),
                w,
                h,
            ];
            for rect in [anchored, centered] {
                if !result.contains(&rect) {
                    result.push(rect);
                }
            }
        }
    }
    result.truncate(32);
    result
}

fn layout_patch(
    snapshot: &Snapshot,
    page: EntityId,
    element: EntityId,
    rect: [f32; 4],
    angle: f32,
    style: crate::commands::project::Typography,
) -> Result<Patch> {
    let style_patch = Project::typography_patch(
        snapshot,
        vec![TypographyUpdate {
            layer: element,
            typography: style,
        }],
    )?;
    let styled = snapshot.preview([&style_patch])?;
    let typography = styled
        .component::<koharu_scene::Typography>(element)?
        .context("candidate style")?;
    let mut layout = snapshot
        .component::<koharu_scene::TextLayout>(element)?
        .context("text layout")?;
    layout.origin = koharu_scene::Origin::User;
    layout.angle_degrees = Some(angle);
    Ok(snapshot.patch(|edit| {
        edit.observe_subtree(page)?;
        edit.promote_entity_to_user(element)?;
        edit.promote_entity_to_user(snapshot.text_layer(element)?.content()?.id())?;
        edit.set(element, &layout)?;
        edit.set(
            element,
            &Geometry::rectangle(
                rect[0] as f64,
                rect[1] as f64,
                rect[2] as f64,
                rect[3] as f64,
            ),
        )?;
        edit.set(element, &typography)
    })?)
}

pub(in crate::commands::agent) async fn search_layouts(
    renderer: &koharu_renderer::Renderer,
    rasterizer: Arc<koharu_rasterizer::Rasterizer>,
    snapshot: &Snapshot,
    page: EntityId,
    query: LayoutSearch,
    before: &ReadabilityReport,
    control: &Control,
) -> Result<LayoutProposals> {
    let element: EntityId =
        serde_json::from_value(json!(query.element)).context("layout element ID")?;
    let measured = before
        .layers
        .iter()
        .find(|l| l.element == element)
        .context("Choose visible audited translated text on this page")?;
    ensure!(
        valid_rect(query.area)
            && contains(
                [
                    0.0,
                    0.0,
                    before.page_width as f32,
                    before.page_height as f32
                ],
                query.area
            ),
        "area must be a positive finite rectangle inside the page"
    );
    ensure!(
        query.avoid.len() <= 16 && query.avoid.iter().all(|r| valid_rect(*r)),
        "Supply at most 16 positive finite protected rectangles"
    );
    let frame = renderer.render(snapshot, page).await?;
    let LayerKind::Text(text) = frame.layer(element).context("rendered text")?.kind() else {
        anyhow::bail!("Not a text layer");
    };
    let direction = text.writing_mode;
    let original_angle = text.angle_degrees;
    let angles = if original_angle.abs() > 0.01 {
        vec![original_angle, 0.0]
    } else {
        vec![original_angle]
    };
    let background = output::frame_image(rasterizer, &frame.without_text()?).await?;
    let original = typography(snapshot, element)?;
    let mut avoid = query.avoid;
    avoid.extend(
        before
            .layers
            .iter()
            .filter(|l| l.element != element)
            .map(|l| l.bounds),
    );
    // Keep floating-point reader scaling from rounding the target below the
    // minimum (e.g. 30.76923 source px becomes 11.999999 at 390/1000).
    let desired = ((before.minimum_font_px + 0.01) * before.page_width as f32
        / 390u32.min(before.page_width).max(1) as f32)
        .max(measured.source_font_px);
    let mut proposals = LayoutProposals {
        page,
        element,
        area: query.area,
        attempts: 0,
        candidates: vec![],
    };
    let mut variants = vec![];
    for rect in frames(query.area, &avoid, measured.layout_bounds) {
        for angle in &angles {
            variants.push((rect, *angle));
        }
    }
    variants.truncate(32);
    for (rect, angle) in variants {
        let mut low = measured.source_font_px;
        let mut high = desired;
        let mut best = None;
        for step in 0..7 {
            ensure!(
                !control.is_cancelled(),
                "Layout search cancelled; no edit was saved"
            );
            let size = if step == 0 {
                high
            } else if step == 6 {
                low
            } else {
                (low + high) * 0.5
            };
            if step > 0 && high - low < 0.2 {
                break;
            }
            let mut metric = measured.clone();
            metric.source_font_px = size;
            let mut style = repair_style(original.clone(), &metric, direction);
            style.size = Some(size);
            style.auto_fit = false;
            let mut patch = layout_patch(snapshot, page, element, rect, angle, style.clone())?;
            let preview = snapshot.preview([&patch])?;
            let mut candidate_frame = renderer.render(&preview, page).await?;
            let mut report = candidate_frame.readability();
            report.sample_background(&background);
            // Resizing changes which artwork sits under the letters. Recompute
            // fill/halo on that exact candidate, never on the old text frame.
            if let Some(layer) = report.layers.iter().find(|l| l.element == element) {
                let adjusted = repair_style(style.clone(), layer, direction);
                if adjusted != style {
                    patch = layout_patch(snapshot, page, element, rect, angle, adjusted)?;
                    let preview = snapshot.preview([&patch])?;
                    candidate_frame = renderer.render(&preview, page).await?;
                    report = candidate_frame.readability();
                    report.sample_background(&background);
                }
            }
            proposals.attempts += 1;
            let layer = report
                .layers
                .iter()
                .find(|l| l.element == element)
                .cloned()
                .context("candidate text disappeared")?;
            let valid = contains(query.area, rect)
                && contains(query.area, layer.bounds)
                && contains(query.area, layer.layout_bounds)
                && avoid.iter().all(|r| {
                    !intersects(*r, rect)
                        && !intersects(*r, layer.bounds)
                        && !intersects(*r, layer.layout_bounds)
                })
                && matches!(candidate_frame.layer(element).map(|l|l.kind()),Some(LayerKind::Text(t)) if t.writing_mode==direction)
                && before.regressions(&report).is_empty()
                && !report.issues.iter().any(|i| {
                    (i.element == element || i.related == Some(element))
                        && (i.kind == ReadabilityKind::Overflow
                            || matches!(
                                i.kind,
                                ReadabilityKind::OverlapCandidate
                                    | ReadabilityKind::DuplicateCandidate
                            ) && before
                                .issues
                                .iter()
                                .find(|old| old.id == i.id)
                                .is_none_or(|old| i.value > old.value + 0.01))
                });
            if valid && layer.source_font_px > measured.source_font_px + 0.25 {
                let displacement = (layer.bounds[0] + layer.bounds[2] * 0.5
                    - measured.bounds[0]
                    - measured.bounds[2] * 0.5)
                    .hypot(
                        layer.bounds[1] + layer.bounds[3] * 0.5
                            - measured.bounds[1]
                            - measured.bounds[3] * 0.5,
                    );
                best = Some(Candidate {
                    id: uuid::Uuid::new_v4().to_string(),
                    patch,
                    frame: candidate_frame,
                    report,
                    geometry: rect,
                    angle_degrees: angle,
                    measured: layer,
                    displacement,
                });
                low = size;
                if step == 0 {
                    break;
                }
            } else {
                high = size;
            }
        }
        if let Some(mut candidate) = best {
            source_duplicates(
                &snapshot.preview([&candidate.patch])?,
                &mut candidate.report,
            )?;
            proposals.candidates.push(candidate);
        }
    }
    proposals.candidates.sort_by(|a, b| {
        b.measured
            .source_font_px
            .total_cmp(&a.measured.source_font_px)
            .then_with(|| {
                (a.angle_degrees - original_angle)
                    .abs()
                    .total_cmp(&(b.angle_degrees - original_angle).abs())
            })
            .then_with(|| a.displacement.total_cmp(&b.displacement))
            .then_with(|| {
                (a.geometry[2] * a.geometry[3]).total_cmp(&(b.geometry[2] * b.geometry[3]))
            })
    });
    let mut distinct: Vec<Candidate> = vec![];
    for c in proposals.candidates {
        if distinct.iter().all(|other| {
            let a = c.measured.bounds;
            let b = other.measured.bounds;
            a.iter().zip(b).any(|(a, b)| (*a - b).abs() > 8.0)
        }) {
            distinct.push(c);
        }
        if distinct.len() == 3 {
            break;
        }
    }
    proposals.candidates = distinct;
    Ok(proposals)
}
