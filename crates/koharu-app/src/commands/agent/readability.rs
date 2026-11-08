//! Per-turn coverage and visual verification. Tool-call counts are not quality checks.
use std::collections::HashMap;

use anyhow::{Context as _, Result, ensure};
use koharu_renderer::{
    Frame, LayerKind, ReadabilityKind, ReadabilityReport, ReadableText, TextAlign, WritingMode,
};
use koharu_scene::{BlobId, EntityId, Geometry, Snapshot};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::commands::{
    editing::TypographyUpdate,
    project::{Project, Typography},
};

mod layout;
pub(super) use layout::{LayoutProposals, LayoutSearch, search_layouts};

#[derive(Clone, Serialize)]
pub(super) struct SourcePlacement {
    pub bounds: [f32; 4],
    pub limit: [f32; 4],
}

impl SourcePlacement {
    pub fn excess(&self, bounds: [f32; 4]) -> f32 {
        let [x, y, w, h] = self.limit;
        (x - bounds[0])
            .max(y - bounds[1])
            .max(bounds[0] + bounds[2] - x - w)
            .max(bounds[1] + bounds[3] - y - h)
            .max(0.0)
    }
}

pub(super) fn source_placement(
    snapshot: &Snapshot,
    element: EntityId,
) -> Result<Option<SourcePlacement>> {
    let content = snapshot.text_layer(element)?.content()?;
    if !content
        .role()?
        .is_some_and(|role| role.role == "dev.koharu.text.free-text")
    {
        return Ok(None);
    }
    let Some(region) = content.source_region()? else {
        return Ok(None);
    };
    let points = region.geometry()?.points;
    if points.is_empty() {
        return Ok(None);
    }
    let b = points.iter().fold(
        [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
        |b, p| [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)],
    );
    let bounds = [
        b[0] as f32,
        b[1] as f32,
        (b[2] - b[0]) as f32,
        (b[3] - b[1]) as f32,
    ];
    if bounds[2] <= 0.0 || bounds[3] <= 0.0 {
        return Ok(None);
    }
    // OCR boxes are approximate. A small allowance accommodates translated
    // line breaks and outlines while retaining the source's composition.
    let limit = [
        bounds[0] - bounds[2] * 0.1,
        bounds[1] - bounds[3] * 0.1,
        bounds[2] * 1.2,
        bounds[3] * 1.2,
    ];
    Ok(Some(SourcePlacement { bounds, limit }))
}

/// Compare original OCR regions even if automatic layout separated the rendered text.
pub(super) fn source_duplicates(
    snapshot: &Snapshot,
    report: &mut ReadabilityReport,
) -> Result<Vec<super::redundancy::RedundantText>> {
    let original = super::redundancy::OriginalText::read(snapshot, report)?;
    let groups = original.groups();
    original.audit(report, &groups);
    Ok(groups)
}

pub(super) fn view_crop(
    snapshot: &Snapshot,
    frame: &Frame,
    element: Option<EntityId>,
    original: bool,
) -> Result<Option<[f32; 4]>> {
    let Some(element) = element else {
        return Ok(None);
    };
    ensure!(
        super::scope::page_for(snapshot, element)? == frame.page(),
        "element is not on this page"
    );
    let bounds = if original {
        let geometry = snapshot
            .text_layer(element)?
            .content()?
            .source_region()?
            .context("layer has no original OCR region; view the full original page")?
            .geometry()?;
        ensure!(!geometry.points.is_empty(), "original OCR region is empty");
        let b = geometry.points.iter().fold(
            [
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ],
            |b, p| [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)],
        );
        [
            b[0] as f32,
            b[1] as f32,
            (b[2] - b[0]) as f32,
            (b[3] - b[1]) as f32,
        ]
    } else {
        let b = frame
            .layer(element)
            .context("element is not rendered on this page")?
            .bounds();
        [b.x, b.y, b.width, b.height]
    };
    Ok(Some([
        bounds[0] - 32.0,
        bounds[1] - 32.0,
        bounds[2] + 64.0,
        bounds[3] + 64.0,
    ]))
}

pub(super) async fn checked_candidate(
    renderer: &koharu_renderer::Renderer,
    rasterizer: std::sync::Arc<koharu_rasterizer::Rasterizer>,
    snapshot: &Snapshot,
    patch: &koharu_scene::Patch,
    page: EntityId,
    before: &ReadabilityReport,
) -> Result<ReadabilityReport> {
    let candidate = snapshot.preview([patch])?;
    let frame = renderer.render(&candidate, page).await?;
    let mut report = frame.readability();
    source_duplicates(&candidate, &mut report)?;
    let background =
        crate::commands::output::frame_image(rasterizer, &frame.without_text()?).await?;
    report.sample_background(&background);
    let mut protected = before.clone();
    let mut placement_errors = vec![];
    for old in &before.layers {
        if let Some(reference) = source_placement(snapshot, old.element)?
            && let Some(next) = report
                .layers
                .iter()
                .find(|layer| layer.element == old.element)
        {
            let old_excess = reference
                .excess(old.bounds)
                .max(reference.excess(old.layout_bounds));
            let next_excess = reference
                .excess(next.bounds)
                .max(reference.excess(next.layout_bounds));
            if next_excess > old_excess + 0.25 {
                placement_errors.push(format!("{}: free text expanded beyond its original lettering footprint. Keep its source scale/placement; the phone-size warning is not a mandatory enlargement target",old.element));
            }
            if old_excess > 0.25 && next_excess <= 0.25 {
                // Correcting an oversized prior edit may reduce its font. The
                // saved result still receives the normal small-text warning.
                protected
                    .layers
                    .iter_mut()
                    .find(|l| l.element == old.element)
                    .unwrap()
                    .display_font_px
                    .clone_from(&next.display_font_px);
            }
        }
    }
    let mut regressions = protected.regressions(&report);
    regressions.extend(placement_errors);
    if !regressions.is_empty() {
        return Err(CandidateRegression(regressions.join("; ")).into());
    }
    Ok(report)
}

#[derive(Debug)]
pub(super) struct CandidateRegression(String);
impl std::fmt::Display for CandidateRegression {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Candidate rejected before saving; no edit was applied. Measured readability regressed: {}. Inspect a crop and preserve layout/contrast.",
            self.0
        )
    }
}
impl std::error::Error for CandidateRegression {}

/// A valid tool proposal declined by review policy is feedback, not a malformed
/// call that can be fixed only by eventually performing the prohibited edit.
#[derive(Debug)]
pub(super) struct DeclinedEdit(pub String);
impl std::fmt::Display for DeclinedEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for DeclinedEdit {}

impl DeclinedEdit {
    pub fn feedback(&self) -> Result<koharu_agent::Invocation> {
        koharu_agent::Invocation::read(serde_json::json!({
            "changed": false,
            "candidate_rejected": self.0,
            "next": "No edit was applied. Do not retry a prohibited deletion or hide the same text instead. Preserve the dialogue, inspect its source and layout, and use repair_text/typography/geometry for readability. Audit and view the current page again, then verify remaining real issues as needs_review with a reason if they cannot be fixed safely. Do not move text or change words to manufacture duplicate evidence."
        }))
    }
}

#[derive(Debug)]
pub(super) struct ReviewRequired(EntityId);

impl std::fmt::Display for ReviewRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Call view_page for the current translated full page {} before editing or verifying; inspect its text and background after the latest change.",
            self.0
        )
    }
}
impl std::error::Error for ReviewRequired {}

impl ReviewRequired {
    pub fn feedback(&self) -> Result<koharu_agent::Invocation> {
        koharu_agent::Invocation::read(serde_json::json!({
            "changed": false,
            "review_required": {
                "tool": "view_page",
                "arguments": {"page": self.0},
                "reason": self.to_string()
            },
            "next": "No edit was applied and no page was verified. Call the indicated view_page without original, element or display_width to refresh the audit and inspect BOTH phone and desktop images of the current translated page. Then reconsider the original edit or verification on the same page/layer targets, using the refreshed issue IDs. An original-artwork view or an element crop does not replace this full-page review."
        }))
    }
}

/// Issue identities describe the current audit, not durable project entities.
/// Reconcile decisions explicitly instead of guessing how obsolete IDs map.
#[derive(Debug)]
pub(super) struct VerificationRequired {
    page: EntityId,
    issues: Vec<koharu_renderer::ReadabilityIssue>,
}

impl std::fmt::Display for VerificationRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Issue decisions do not match the current audit for page {}. No page was verified.",
            self.page
        )
    }
}
impl std::error::Error for VerificationRequired {}

impl VerificationRequired {
    pub fn feedback(&self) -> Result<koharu_agent::Invocation> {
        koharu_agent::Invocation::read(serde_json::json!({
            "changed": false,
            "verified": false,
            "verification_required": {
                "page": self.page,
                "issues": self.issues,
                "reason": self.to_string()
            },
            "next": "Reconsider each current issue on this SAME page, then call verify_readability with exactly one decision per id copied verbatim from verification_required.issues. These issue IDs are not layer IDs and their spelling must not be reconstructed from kind or element. Use needs_review with a concrete reason for real unresolved defects; keep is only for visual collision/duplicate false positives. If you edit the page, view_page again and use its refreshed issue list. This response saved no edit and did not verify the page."
        }))
    }
}

pub(super) struct RepairBatch {
    pub patch: koharu_scene::Patch,
    pub report: ReadabilityReport,
    pub elements: Vec<EntityId>,
    pub removed_duplicates: Vec<EntityId>,
    pub balanced_groups: Vec<Vec<EntityId>>,
    pub declined: Vec<String>,
}

fn typography(snapshot: &Snapshot, element: EntityId) -> Result<Typography> {
    Ok(
        if let Some(value) = snapshot.text_layer(element)?.typography()? {
            serde_json::from_value(serde_json::to_value(value)?)?
        } else {
            serde_json::from_value(serde_json::json!({"auto_fit":true}))?
        },
    )
}

#[derive(PartialEq)]
struct DialogueStyle {
    balloon: EntityId,
    typography: Typography,
    alignment: TextAlign,
    direction: WritingMode,
}

fn lettering_groups(
    snapshot: &Snapshot,
    frame: &Frame,
    report: &ReadabilityReport,
    requested: &[EntityId],
) -> Result<Vec<Vec<EntityId>>> {
    let mut groups: Vec<(Option<DialogueStyle>, Vec<EntityId>)> = vec![];
    for entry in &report.layers {
        let id = entry.element;
        if !requested.is_empty() && !requested.contains(&id) {
            continue;
        }
        let layer = snapshot.text_layer(id)?;
        let style = if let Some(balloon) = layer.balloon_target()?
            && snapshot.component::<Geometry>(id)?.is_none()
            && layer
                .content()?
                .role()?
                .is_some_and(|role| role.role == "dev.koharu.text.dialogue")
            && let Some(LayerKind::Text(rendered)) = frame.layer(id).map(|layer| layer.kind())
        {
            let mut typography = typography(snapshot, id)?;
            typography.size = None;
            typography.auto_fit = true;
            typography.color = Some(rendered.color);
            typography.alignment = None;
            typography.writing_mode = None;
            Some(DialogueStyle {
                balloon: balloon.id(),
                typography,
                alignment: rendered.alignment,
                direction: rendered.writing_mode,
            })
        } else {
            None
        };
        if style.is_some()
            && let Some((_, elements)) = groups.iter_mut().find(|(key, _)| *key == style)
        {
            elements.push(id);
        } else {
            groups.push((style, vec![id]));
        }
    }
    Ok(groups.into_iter().map(|(_, elements)| elements).collect())
}

fn balance_floor(report: &mut ReadabilityReport, group: &[EntityId]) {
    let Some(smallest) = report
        .layers
        .iter()
        .filter(|layer| group.contains(&layer.element))
        .min_by(|left, right| left.source_font_px.total_cmp(&right.source_font_px))
        .map(|layer| layer.display_font_px.clone())
    else {
        return;
    };
    // Equalizing the existing dialogue can reduce an outlier. The smallest
    // size may only improve, and text meeting the reader minimum keeps it.
    for layer in &mut report.layers {
        if group.contains(&layer.element) && layer.display_font_px[0] < report.minimum_font_px {
            layer.display_font_px.clone_from(&smallest);
        }
    }
}

fn repair_style(
    mut typography: Typography,
    layer: &ReadableText,
    direction: WritingMode,
) -> Typography {
    // Detection can infer vertical source lettering that the renderer ignores
    // for a Latin translation. Promoting the style must not activate it.
    if typography.writing_mode.is_some_and(|mode| {
        (mode == koharu_scene::WritingMode::Vertical) != direction.is_vertical()
    }) {
        typography.writing_mode = None;
    }
    if let Some(bg) = &layer.background {
        let thick = layer.stroke_width > layer.maximum_outline_px();
        if bg.required_outline_px > 0.0 {
            let neutral = |color: [u8; 4]| {
                color[..3].iter().max().unwrap() - color[..3].iter().min().unwrap() < 16
            };
            if neutral(layer.color) && layer.stroke_color.is_none_or(neutral) {
                // Background luminance cannot identify the source lettering's
                // polarity. Keep the letter faces and strengthen their halo.
                if layer.stroke_color.is_none() || bg.paired_contrast < 4.5 {
                    let light_fill =
                        layer.color[..3].iter().map(|c| u16::from(*c)).sum::<u16>() >= 384;
                    typography.stroke_color = Some(if light_fill {
                        [0, 0, 0, 255]
                    } else {
                        [255, 255, 255, 255]
                    });
                }
            } else if layer.stroke_color.is_none() {
                typography.stroke_color = Some(if bg.dark_fraction >= 0.5 {
                    [255, 255, 255, 255]
                } else {
                    [0, 0, 0, 255]
                });
            }
            typography.stroke_width = Some(layer.outline_target_px());
        } else if bg.light_fraction > 0.95 || bg.dark_fraction > 0.95 {
            let replace_fill = bg.fill_contrast < 4.5 && bg.outline_contrast < 4.5;
            if replace_fill {
                typography.color = Some(if bg.light_fraction > 0.95 {
                    [0, 0, 0, 255]
                } else {
                    [255, 255, 255, 255]
                });
            }
            if thick {
                // A colored title may rely on its outline for contrast.
                typography.stroke_width = Some(if bg.fill_contrast >= 4.5 || replace_fill {
                    0.0
                } else {
                    layer.source_font_px * 0.035
                });
            }
        } else if thick {
            typography.stroke_width = Some(layer.source_font_px * 0.035);
        }
    }
    typography
}

/// Find the largest common size that fits the existing cells of one balloon.
/// A single layer uses the same search and its own existing size as the floor.
async fn repair(
    renderer: &koharu_renderer::Renderer,
    rasterizer: std::sync::Arc<koharu_rasterizer::Rasterizer>,
    snapshot: &Snapshot,
    frame: &Frame,
    elements: &[EntityId],
    before: &ReadabilityReport,
    background: &image::RgbaImage,
) -> Result<(koharu_scene::Patch, bool)> {
    let shared = elements.len() > 1;
    let mut styles = vec![];
    let mut low = f32::INFINITY;
    let mut largest = 0.0f32;
    for element in elements {
        let layer = before
            .layers
            .iter()
            .find(|l| l.element == *element)
            .context("target must be visible translated text")?;
        let LayerKind::Text(rendered) =
            frame.layer(*element).context("rendered text layer")?.kind()
        else {
            anyhow::bail!("repair target is not rendered text");
        };
        let original = typography(snapshot, *element)?;
        styles.push((*element, original, layer.clone(), rendered.writing_mode));
        low = low.min(layer.source_font_px);
        largest = largest.max(layer.source_font_px);
    }
    let patch_for = |size: Option<f32>| {
        Project::typography_patch(
            snapshot,
            styles
                .iter()
                .filter_map(|(layer, original, measured, direction)| {
                    let mut measured = measured.clone();
                    if let Some(size) = size {
                        measured.source_font_px = size;
                    }
                    let mut typography = repair_style(original.clone(), &measured, *direction);
                    if let Some(size) = size {
                        typography.size = Some(size);
                        typography.auto_fit = false;
                    }
                    (typography != *original).then_some(TypographyUpdate {
                        layer: *layer,
                        typography,
                    })
                })
                .collect(),
        )
    };
    let mut protected = before.clone();
    if shared {
        balance_floor(&mut protected, elements);
    }
    let desired = before.minimum_font_px * before.page_width as f32
        / koharu_renderer::READER_WIDTHS[0]
            .min(before.page_width)
            .max(1) as f32;
    let mut high = desired.max(largest);
    let mut best = None;
    for step in 0..8 {
        let at_floor = step == 7 || high - low < 0.25;
        let size = if at_floor {
            if !shared || best.is_some() || largest - low < 0.25 {
                break;
            }
            low
        } else if step == 0 {
            high
        } else {
            (low + high) * 0.5
        };
        let patch = patch_for(Some(size))?;
        let candidate = snapshot.preview([&patch])?;
        let candidate_frame = renderer.render(&candidate, before.page).await?;
        let direction_preserved = styles.iter().all(|(id, _, _, direction)| {
            matches!(candidate_frame.layer(*id).map(|layer| layer.kind()),
                Some(LayerKind::Text(text)) if text.writing_mode == *direction)
        });
        let mut report = candidate_frame.readability();
        report.sample_background(background);
        let new_overlap = report.issues.iter().any(|i| {
            matches!(
                i.kind,
                ReadabilityKind::OverlapCandidate | ReadabilityKind::DuplicateCandidate
            ) && (elements.contains(&i.element)
                || i.related.is_some_and(|id| elements.contains(&id)))
                && before
                    .issues
                    .iter()
                    .find(|old| old.id == i.id)
                    .is_none_or(|old| i.value > old.value + 0.01)
        });
        if direction_preserved
            && protected.regressions(&report).is_empty()
            && !new_overlap
            && !report
                .issues
                .iter()
                .any(|i| elements.contains(&i.element) && i.kind == ReadabilityKind::Overflow)
        {
            best = Some(patch);
            low = size;
            if size == high {
                break;
            }
        } else {
            high = size;
        }
        if at_floor {
            break;
        }
    }
    let balanced = shared && best.is_some();
    let patch = match best {
        Some(patch) => patch,
        None => patch_for(None)?,
    };
    if !patch.is_empty() {
        let baseline = if balanced { &protected } else { before };
        checked_candidate(
            renderer,
            rasterizer,
            snapshot,
            &patch,
            before.page,
            baseline,
        )
        .await?;
    }
    Ok((patch, balanced))
}

pub(super) async fn repair_batch(
    renderer: &koharu_renderer::Renderer,
    rasterizer: std::sync::Arc<koharu_rasterizer::Rasterizer>,
    snapshot: &Snapshot,
    elements: &[EntityId],
    before: &ReadabilityReport,
) -> Result<RepairBatch> {
    let elements = if elements.is_empty() {
        before.layers.iter().map(|layer| layer.element).collect()
    } else {
        elements.to_vec()
    };
    ensure!(
        elements.len() <= 50,
        "Choose up to 50 audited text layers on one page"
    );
    if elements.is_empty() {
        return Ok(RepairBatch {
            patch: snapshot.patch(|_| Ok(()))?,
            report: before.clone(),
            elements,
            removed_duplicates: vec![],
            balanced_groups: vec![],
            declined: vec![],
        });
    }
    let mut seen = std::collections::HashSet::new();
    for element in &elements {
        ensure!(seen.insert(*element), "Duplicate element ID");
        ensure!(
            before.layers.iter().any(|layer| layer.element == *element),
            "Batch layers must be audited, visible translated text on the requested page"
        );
    }
    let duplicates = super::redundancy::OriginalText::read(snapshot, before)?
        .groups()
        .into_iter()
        .filter(|group| {
            group.automatic
                && seen.contains(&group.element)
                && group
                    .restore
                    .as_ref()
                    .is_none_or(|restore| elements.contains(&restore.element))
        })
        .collect::<Vec<_>>();
    let removed_duplicates = duplicates
        .iter()
        .map(|group| group.element)
        .collect::<Vec<_>>();
    let removal = snapshot.patch(|edit| {
        for group in &duplicates {
            group.apply(snapshot, edit)?;
        }
        Ok(())
    })?;
    let mut preview = snapshot.preview([&removal])?;
    let background = crate::commands::output::frame_image(
        rasterizer.clone(),
        &renderer
            .render(snapshot, before.page)
            .await?
            .without_text()?,
    )
    .await?;
    let frame = renderer.render(&preview, before.page).await?;
    let current = frame.readability();
    let groups = lettering_groups(&preview, &frame, &current, &elements)?;
    let mut updates = vec![];
    let mut balanced_groups = vec![];
    let mut declined = vec![];
    for group in groups {
        let frame = renderer.render(&preview, before.page).await?;
        let mut current = frame.readability();
        current.sample_background(&background);
        let (patch, balanced) = match repair(
            renderer,
            rasterizer.clone(),
            &preview,
            &frame,
            &group,
            &current,
            &background,
        )
        .await
        {
            Ok(result) => result,
            Err(error) if error.is::<CandidateRegression>() => {
                declined.push(format!("{group:?}: {error}"));
                continue;
            }
            Err(error) => return Err(error),
        };
        if balanced {
            balanced_groups.push(group.clone());
        }
        if patch.is_empty() {
            continue;
        }
        preview = preview.preview([&patch])?;
        updates.extend(group);
    }
    let patch = snapshot.patch(|edit| {
        edit.observe_subtree(before.page)?;
        for group in &duplicates {
            group.apply(snapshot, edit)?;
        }
        for id in &updates {
            let layer = preview.text_layer(*id)?;
            edit.promote_entity_to_user(*id)?;
            edit.promote_entity_to_user(layer.content()?.id())?;
            let style = layer.typography()?.ok_or_else(|| {
                koharu_scene::Error::Invalid("repaired typography is missing".into())
            })?;
            edit.set(*id, &style)?;
        }
        Ok(())
    })?;
    let mut protected = before.clone();
    protected
        .layers
        .retain(|layer| !removed_duplicates.contains(&layer.element));
    protected.issues.retain(|issue| {
        !removed_duplicates.contains(&issue.element)
            && issue
                .related
                .is_none_or(|id| !removed_duplicates.contains(&id))
    });
    for group in &balanced_groups {
        balance_floor(&mut protected, group);
    }
    let report = checked_candidate(
        renderer,
        rasterizer,
        snapshot,
        &patch,
        before.page,
        &protected,
    )
    .await?;
    Ok(RepairBatch {
        patch,
        report,
        elements,
        removed_duplicates,
        balanced_groups,
        declined,
    })
}

#[derive(Clone, Debug, Default, Serialize, Type)]
pub struct ReadabilityProgress {
    pub total: u32,
    pub checked: u32,
    pub needs_review: u32,
}

#[derive(Default)]
pub(super) struct Review {
    scope: Option<Vec<EntityId>>,
    pages: HashMap<EntityId, PageReview>,
    source_views: HashMap<EntityId, BlobId>,
    layouts: Option<LayoutProposals>,
}

struct PageReview {
    stamp: BlobId,
    report: ReadabilityReport,
    viewed: bool,
    phone: bool,
    desktop: bool,
    verified: bool,
    decisions: Vec<Decision>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct VerifyReadability {
    /// Actual PAGE ID from inspect_chapter or view_page, never a text layer/region ID.
    pub page: String,
    /// One decision for EVERY current issue; copy its exact id from the latest
    /// full view_page or verification_required.issues (not its kind or element).
    /// Never label small text acceptable merely because it fits its box.
    pub issues: Vec<Decision>,
}

impl VerifyReadability {
    pub fn resolve_page(&self, snapshot: &Snapshot) -> Result<EntityId> {
        let page: EntityId = serde_json::from_value(serde_json::json!(self.page))
            .context("verify_readability.page must be an inspected page ID")?;
        if snapshot.page(page).is_ok() {
            return Ok(page);
        }
        if let Ok(owner) = super::scope::page_for(snapshot, page) {
            anyhow::bail!(
                "verify_readability.page requires a PAGE ID, but {page} is a layer or other entity on {}. Use page=\"{owner}\" and the current issue IDs from audit_readability/view_page. No page was verified.",
                super::context::Locations::new(snapshot)?.label(owner)?
            );
        }
        anyhow::bail!(
            "verify_readability.page {page} is not a current page. Read inspect_chapter for the actual page ID, then audit/view that page before verifying. No page was verified."
        )
    }
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct Decision {
    pub issue: String,
    pub disposition: Disposition,
    /// Explain the visual evidence, or why the page still needs manual review.
    pub reason: String,
}

#[derive(Clone, Copy, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Disposition {
    /// Only for a visual false positive (e.g. rotated bounding rectangles overlap).
    Keep,
    /// A real problem remains; include it in the final answer, never claim repaired.
    NeedsReview,
}

pub(super) fn stamp(snapshot: &Snapshot, page: EntityId) -> Result<BlobId> {
    Ok(BlobId::for_bytes(&serde_json::to_vec(&Project::page(
        snapshot, page,
    )?)?))
}

impl Review {
    pub fn propose_layouts(&mut self, proposals: LayoutProposals) {
        self.layouts = Some(proposals);
    }

    pub fn layout_choice(
        &self,
        snapshot: &Snapshot,
        page: EntityId,
        candidate: &str,
    ) -> Result<(koharu_scene::Patch, serde_json::Value)> {
        self.before_edit(page, stamp(snapshot, page)?)?
            .context("Call audit_readability and view_page before choosing a layout")?;
        self.layouts
            .as_ref()
            .context("Call repair_text with layout to preview candidates first")?
            .choose(snapshot, page, candidate)
    }

    pub fn begin(&mut self, pages: &[EntityId]) -> Result<()> {
        if let Some(scope) = &self.scope {
            ensure!(
                scope == pages,
                "A readability review cannot change its range mid-turn. Finish the original range."
            );
        } else {
            self.scope = Some(pages.to_vec());
        }
        Ok(())
    }

    pub fn active(&self) -> bool {
        self.scope.is_some()
    }

    pub fn audit(&mut self, stamp: BlobId, report: ReadabilityReport) {
        if self
            .pages
            .get(&report.page)
            .is_some_and(|review| review.stamp == stamp && review.report == report)
        {
            return;
        }
        let page = report.page;
        self.pages.insert(
            page,
            PageReview {
                stamp,
                report,
                viewed: false,
                phone: false,
                desktop: false,
                verified: false,
                decisions: vec![],
            },
        );
    }

    pub fn before_edit(&self, page: EntityId, stamp: BlobId) -> Result<Option<ReadabilityReport>> {
        if !self.active() {
            return Ok(None);
        }
        let audit = self.pages.get(&page).ok_or(ReviewRequired(page))?;
        if audit.stamp != stamp || !audit.viewed {
            return Err(ReviewRequired(page).into());
        }
        Ok(Some(audit.report.clone()))
    }

    pub fn changed(&mut self, page: EntityId) {
        self.pages.remove(&page);
    }

    pub fn source_viewed(&mut self, page: EntityId, stamp: BlobId) {
        self.source_views.insert(page, stamp);
    }

    pub fn source_was_viewed(&self, page: EntityId, stamp: BlobId) -> bool {
        self.source_views.get(&page) == Some(&stamp)
    }

    pub fn viewed(&mut self, page: EntityId, stamp: BlobId, width: Option<u32>) {
        if let Some(audit) = self.pages.get_mut(&page) {
            if audit.stamp == stamp {
                audit.viewed = true;
                audit.phone |= width.is_none() || width == Some(390);
                audit.desktop |= width.is_none() || width == Some(960);
            }
        }
    }

    pub fn verify(
        &mut self,
        page: EntityId,
        stamp: BlobId,
        decisions: Vec<Decision>,
    ) -> Result<()> {
        let audit = self.pages.get_mut(&page).ok_or(ReviewRequired(page))?;
        if audit.stamp != stamp || !(audit.viewed && audit.phone && audit.desktop) {
            return Err(ReviewRequired(page).into());
        }
        let mut seen = std::collections::HashSet::new();
        for decision in &decisions {
            ensure!(seen.insert(&decision.issue), "Duplicate issue decision");
            ensure!(
                (12..=1000).contains(&decision.reason.trim().len()),
                "Give a concrete reason (12–1000 bytes)"
            );
        }
        let issues = decisions
            .iter()
            .map(|decision| audit.report.issues.iter().find(|i| i.id == decision.issue))
            .collect::<Option<Vec<_>>>();
        let Some(issues) = issues.filter(|_| decisions.len() == audit.report.issues.len()) else {
            return Err(VerificationRequired {
                page,
                issues: audit.report.issues.clone(),
            }
            .into());
        };
        for (decision, issue) in decisions.iter().zip(issues) {
            ensure!(
                decision.disposition != Disposition::Keep
                    || matches!(
                        issue.kind,
                        ReadabilityKind::OverlapCandidate | ReadabilityKind::DuplicateCandidate
                    ),
                "Measured small text, overflow, low contrast or thick outlines remain needs_review until fixed. Keep is only for visual collision/duplicate false positives."
            );
        }
        audit.decisions = decisions;
        audit.verified = true;
        Ok(())
    }

    pub fn progress(&self) -> Option<ReadabilityProgress> {
        let scope = self.scope.as_ref()?;
        Some(ReadabilityProgress {
            total: scope.len() as u32,
            checked: self.pages.values().filter(|p| p.verified).count() as u32,
            needs_review: self
                .pages
                .values()
                .flat_map(|p| &p.decisions)
                .filter(|d| d.disposition == Disposition::NeedsReview)
                .count() as u32,
        })
    }

    pub fn ensure_complete(&self) -> Result<()> {
        if let Some(scope) = &self.scope {
            let pending = scope
                .iter()
                .filter(|p| !self.pages.get(p).is_some_and(|p| p.verified))
                .collect::<Vec<_>>();
            ensure!(
                pending.is_empty(),
                "Readability review is incomplete: {} of {} pages still require audit_readability, view_page and verify_readability. Next page IDs: {:?}",
                pending.len(),
                scope.len(),
                &pending[..pending.len().min(8)]
            );
        }
        Ok(())
    }

    pub fn ensure_fresh(&self, snapshot: &Snapshot) -> Result<()> {
        for (page, review) in &self.pages {
            ensure!(
                !review.verified || stamp(snapshot, *page)? == review.stamp,
                "Verified page {page} changed after review; audit, view and verify it again before completion."
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn outline_repair_keeps_black_letter_faces_inside_white_halos_on_dark_art() -> Result<()> {
        let style: Typography = serde_json::from_value(serde_json::json!({
            "auto_fit":false,"size":24.0,"color":[0,0,0,255],"stroke_color":[255,255,255,255],"stroke_width":0.5
        }))?;
        let mut measured = ReadableText {
            element: EntityId::new(),
            text: "Narration on artwork".into(),
            source_font_px: 24.0,
            display_font_px: vec![9.75, 24.0],
            fonts: vec![],
            bounds: [0.0, 0.0, 100.0, 100.0],
            layout_bounds: [0.0, 0.0, 100.0, 100.0],
            color: [0, 0, 0, 255],
            stroke_color: Some([255, 255, 255, 255]),
            stroke_width: 0.5,
            opacity: 1.0,
            background: None,
        };
        for dark_fraction in [0.3, 0.8, 1.0] {
            measured.background = Some(koharu_renderer::Background {
                dark_fraction,
                light_fraction: 1.0 - dark_fraction,
                luminance_spread: 1.0,
                fill_contrast: 1.0,
                outline_contrast: if dark_fraction == 1.0 { 21.0 } else { 1.0 },
                paired_contrast: 21.0,
                required_outline_px: 960.0 / 390.0,
            });
            let repaired = repair_style(style.clone(), &measured, WritingMode::Horizontal);
            assert_eq!(repaired.color, style.color, "retain the black letter faces");
            assert_eq!(
                repaired.stroke_color, style.stroke_color,
                "retain the white halo"
            );
            assert!(repaired.stroke_width.unwrap() > measured.stroke_width);
        }
        Ok(())
    }
    #[test]
    fn outline_repair_preserves_colored_lettering_palette() -> Result<()> {
        let style: Typography = serde_json::from_value(serde_json::json!({
            "auto_fit":false,"size":24.0,"color":[255,255,255,255],"stroke_color":[141,57,114,255],"stroke_width":4.0
        }))?;
        let measured = ReadableText {
            element: EntityId::new(),
            text: "Colored title".into(),
            source_font_px: 24.0,
            display_font_px: vec![9.75, 24.0],
            fonts: vec![],
            bounds: [0.0, 0.0, 100.0, 100.0],
            layout_bounds: [0.0, 0.0, 100.0, 100.0],
            color: [255, 255, 255, 255],
            stroke_color: Some([141, 57, 114, 255]),
            stroke_width: 4.0,
            opacity: 1.0,
            background: Some(koharu_renderer::Background {
                dark_fraction: 0.3,
                light_fraction: 0.2,
                luminance_spread: 0.9,
                fill_contrast: 1.0,
                outline_contrast: 2.0,
                paired_contrast: 3.2,
                required_outline_px: 960.0 / 390.0,
            }),
        };
        let repaired = repair_style(style.clone(), &measured, WritingMode::Horizontal);
        assert_eq!(repaired.color, style.color);
        assert_eq!(repaired.stroke_color, style.stroke_color);
        assert!(repaired.stroke_width.unwrap() < 4.0);
        Ok(())
    }
    fn report(page: EntityId) -> ReadabilityReport {
        ReadabilityReport {
            page,
            page_width: 1000,
            page_height: 1400,
            reader_widths: vec![390, 960],
            minimum_font_px: 12.0,
            layers: vec![],
            issues: vec![],
        }
    }
    #[test]
    fn original_artwork_review_never_counts_as_translated_page_verification() -> Result<()> {
        let page = EntityId::new();
        let version = BlobId::for_bytes(b"before");
        let mut review = Review::default();
        review.begin(&[page])?;
        review.audit(version, report(page));
        review.source_viewed(page, version);
        assert!(review.source_was_viewed(page, version));
        assert!(review.before_edit(page, version).is_err());
        assert!(review.verify(page, version, vec![]).is_err());
        review.viewed(page, version, None);
        review.verify(page, version, vec![])?;
        assert!(!review.source_was_viewed(page, BlobId::for_bytes(b"changed")));
        Ok(())
    }
    #[test]
    fn coverage_requires_each_scoped_page_and_a_fresh_view_after_edits() -> Result<()> {
        let pages = [EntityId::new(), EntityId::new()];
        let stamp = BlobId::for_bytes(b"before");
        let mut review = Review::default();
        review.begin(&pages)?;
        review.audit(stamp, report(pages[0]));
        assert!(review.before_edit(pages[0], stamp).is_err());
        assert!(review.verify(pages[0], stamp, vec![]).is_err());
        review.viewed(pages[0], stamp, Some(960));
        assert!(
            review.verify(pages[0], stamp, vec![]).is_err(),
            "desktop alone is not phone verification"
        );
        review.viewed(pages[0], stamp, Some(390));
        review.verify(pages[0], stamp, vec![])?;
        review.audit(stamp, report(pages[0]));
        review.verify(pages[0], stamp, vec![])?;
        assert!(
            review.ensure_complete().is_err(),
            "one page is not the project"
        );
        assert!(
            review.begin(&pages[..1]).is_err(),
            "cannot hide unread pages by narrowing scope"
        );
        review.changed(pages[0]);
        assert!(review.before_edit(pages[0], stamp).is_err());
        for page in pages {
            review.audit(stamp, report(page));
            review.viewed(page, stamp, None);
            review.verify(page, stamp, vec![])?;
        }
        review.ensure_complete()?;
        assert_eq!(review.progress().unwrap().checked, 2);
        Ok(())
    }

    #[test]
    fn measured_defects_cannot_be_silently_accepted_as_false_positives() -> Result<()> {
        let page = EntityId::new();
        let element = EntityId::new();
        let stamp = BlobId::for_bytes(b"a");
        let mut report = report(page);
        report.issues.push(koharu_renderer::ReadabilityIssue {
            id: "small".into(),
            element,
            related: None,
            kind: ReadabilityKind::SmallText,
            value: 4.0,
        });
        let mut review = Review::default();
        review.begin(&[page])?;
        review.audit(stamp, report);
        review.viewed(page, stamp, None);
        let decision = Decision {
            issue: "small".into(),
            disposition: Disposition::Keep,
            reason: "It fits inside the balloon".into(),
        };
        let error = review.verify(page, stamp, vec![decision]).unwrap_err();
        assert!(
            !error.is::<VerificationRequired>() && !error.is::<ReviewRequired>(),
            "a false claim that a measured defect is acceptable remains a validation failure"
        );
        review.verify(
            page,
            stamp,
            vec![Decision {
                issue: "small".into(),
                disposition: Disposition::NeedsReview,
                reason: "Cannot enlarge without crossing the panel; requires a new layout".into(),
            }],
        )?;
        review.ensure_complete()?;
        assert_eq!(review.progress().unwrap().needs_review, 1);
        Ok(())
    }

    #[test]
    fn duplicate_issue_decisions_are_not_an_audit_refresh() -> Result<()> {
        let page = EntityId::new();
        let stamp = BlobId::for_bytes(b"current");
        let mut report = report(page);
        for id in ["small-1", "small-2"] {
            report.issues.push(koharu_renderer::ReadabilityIssue {
                id: id.into(),
                element: EntityId::new(),
                related: None,
                kind: ReadabilityKind::SmallText,
                value: 4.0,
            });
        }
        let mut review = Review::default();
        review.begin(&[page])?;
        review.audit(stamp, report);
        review.viewed(page, stamp, None);
        let decision = Decision {
            issue: "small-1".into(),
            disposition: Disposition::NeedsReview,
            reason: "This lettering still needs visual layout review.".into(),
        };
        let error = review
            .verify(page, stamp, vec![decision.clone(), decision])
            .unwrap_err();
        assert!(error.to_string().contains("Duplicate issue decision"));
        assert!(!error.is::<VerificationRequired>() && !error.is::<ReviewRequired>());
        assert_eq!(review.progress().unwrap().checked, 0);
        assert!(review.ensure_complete().is_err());
        Ok(())
    }

    #[tokio::test]
    async fn completion_rechecks_page_state_without_invalidating_an_unrelated_page() -> Result<()> {
        use koharu_scene::{At, PageDraft, Session};
        let mut session = Session::memory().await?;
        let mut pages = vec![];
        let patch = session.snapshot().patch(|edit| {
            for label in ["001", "002"] {
                pages.push(edit.add_page(PageDraft::new(label, 1000.0, 1400.0), At::End)?);
            }
            Ok(())
        })?;
        let snapshot = session.commit(patch).await?.snapshot;
        let version = stamp(&snapshot, pages[0])?;
        let mut review = Review::default();
        review.begin(&pages[..1])?;
        review.audit(version, report(pages[0]));
        review.viewed(pages[0], version, None);
        review.verify(pages[0], version, vec![])?;
        for (page, expected) in [(pages[1], true), (pages[0], false)] {
            let snapshot = session.snapshot();
            let mut value = snapshot.page(page)?.page()?;
            value.width += 100.0;
            let patch = snapshot.patch(|edit| {
                edit.set_page(page, PageDraft::new(value.label, value.width, value.height))
            })?;
            let snapshot = session.commit(patch).await?.snapshot;
            assert_eq!(review.ensure_fresh(&snapshot).is_ok(), expected);
        }
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Manual capture of an already edited detached review sample"]
    async fn capture_detached_agent_review() -> Result<()> {
        let source =
            std::path::PathBuf::from(std::env::var("KOHARU_READABILITY_SAMPLE")?).canonicalize()?;
        ensure!(
            source.starts_with(
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../temp")
                    .canonicalize()?
            ),
            "Only detached samples under repo temp can be opened"
        );
        let output = std::path::PathBuf::from(std::env::var("KOHARU_READABILITY_OUTPUT")?);
        std::fs::create_dir_all(&output)?;
        koharu_runtime::Store::configure(std::path::PathBuf::from(std::env::var(
            "KOHARU_NATIVE_ACCEPTANCE_STORE",
        )?))?;
        let project = Project::open("review capture".into(), source).await?;
        let snapshot = project.snapshot();
        let positions: Vec<[usize; 2]> =
            serde_json::from_str(&std::env::var("KOHARU_READABILITY_POSITIONS")?)?;
        let chapters = snapshot.chapters()?;
        let pages = positions
            .iter()
            .map(|[chapter, page]| {
                chapters
                    .entries
                    .get(chapter.saturating_sub(1))
                    .and_then(|c| c.pages.get(page.saturating_sub(1)))
                    .copied()
                    .context("requested chapter/page")
            })
            .collect::<Result<Vec<_>>>()?;
        let renderer = koharu_renderer::Renderer::new()?;
        let rasterizer = std::sync::Arc::new(koharu_rasterizer::Rasterizer::new()?);
        let mut reports = vec![];
        for page in pages {
            std::fs::write(
                output.join(format!("page-{page}-layers.json")),
                serde_json::to_vec_pretty(&Project::page(&snapshot, page)?)?,
            )?;
            let frame = renderer.render(&snapshot, page).await?;
            for width in [390, 960] {
                let bytes = crate::commands::output::frame_preview(
                    rasterizer.clone(),
                    &frame,
                    None,
                    Some(width),
                )
                .await?;
                std::fs::write(output.join(format!("page-{page}-{width}.webp")), bytes)?;
                let original = crate::commands::output::frame_preview(
                    rasterizer.clone(),
                    &frame.original_artwork()?,
                    None,
                    Some(width),
                )
                .await?;
                std::fs::write(
                    output.join(format!("page-{page}-original-{width}.webp")),
                    original,
                )?;
            }
            let mut report = frame.readability();
            source_duplicates(&snapshot, &mut report)?;
            report.sample_background(
                &crate::commands::output::frame_image(rasterizer.clone(), &frame.without_text()?)
                    .await?,
            );
            reports.push(report);
        }
        std::fs::write(
            output.join("captured-readability.json"),
            serde_json::to_vec_pretty(&reports)?,
        )?;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Manual readability validation on a detached user-project copy in the repository temp directory"]
    async fn detached_manga_pages_keep_artwork_words_and_format_while_improving_measured_text()
    -> Result<()> {
        use std::{path::PathBuf, sync::Arc};
        let source = PathBuf::from(std::env::var("KOHARU_READABILITY_SAMPLE")?).canonicalize()?;
        let output = PathBuf::from(std::env::var("KOHARU_READABILITY_OUTPUT")?);
        ensure!(
            source.starts_with(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../temp")
                    .canonicalize()?
            ),
            "Only a detached copy under repository temp can be edited by this fixture"
        );
        std::fs::create_dir_all(&output)?;
        koharu_runtime::Store::configure(PathBuf::from(std::env::var(
            "KOHARU_NATIVE_ACCEPTANCE_STORE",
        )?))?;
        let mut project = Project::open("detached readability validation".into(), source).await?;
        let chapters = project.snapshot().chapters()?;
        let requested: Vec<[usize; 2]> = match std::env::var("KOHARU_READABILITY_POSITIONS") {
            Ok(value) => serde_json::from_str(&value)?,
            Err(std::env::VarError::NotPresent) => vec![[1, 1], [1, 16], [2, 2], [2, 4], [3, 1]],
            Err(error) => return Err(error.into()),
        };
        let renderer = koharu_renderer::Renderer::new()?;
        let rasterizer = Arc::new(koharu_rasterizer::Rasterizer::new()?);
        let mut evidence = vec![];
        for (sample, [chapter, index]) in requested.into_iter().enumerate() {
            let page = *chapters
                .entries
                .get(chapter.checked_sub(1).context("chapter must be 1-based")?)
                .context("chapter")?
                .pages
                .get(index.checked_sub(1).context("page must be 1-based")?)
                .context("page")?;
            let initial = project.snapshot();
            let before_frame = renderer.render(&initial, page).await?;
            let background = crate::commands::output::frame_image(
                rasterizer.clone(),
                &before_frame.without_text()?,
            )
            .await?;
            let mut before = before_frame.readability();
            let duplicates = source_duplicates(&initial, &mut before)?;
            before.sample_background(&background);
            // The guard is deliberately exercised with an actual destructive candidate.
            if let Some(layer) = before.layers.first() {
                let mut style: crate::commands::project::Typography =
                    serde_json::from_value(serde_json::to_value(
                        initial
                            .text_layer(layer.element)?
                            .typography()?
                            .context("style")?,
                    )?)?;
                style.size = Some((layer.source_font_px * 0.4).max(1.0));
                style.auto_fit = false;
                style.stroke_width = Some(layer.source_font_px);
                let patch = Project::typography_patch(
                    &initial,
                    vec![crate::commands::editing::TypographyUpdate {
                        layer: layer.element,
                        typography: style,
                    }],
                )?;
                let revision = project.revision();
                ensure!(
                    checked_candidate(
                        &renderer,
                        rasterizer.clone(),
                        &initial,
                        &patch,
                        page,
                        &before
                    )
                    .await
                    .is_err(),
                    "a smaller font with huge outline must be rejected"
                );
                ensure!(project.revision() == revision, "rejected preview was saved");
            }
            let batch = repair_batch(&renderer, rasterizer.clone(), &initial, &[], &before).await?;
            let commit = project.commit(batch.patch).await?;
            let changes = u32::from(commit.changes.from != commit.changes.to);
            project.record_commit(&commit);
            let final_snapshot = project.snapshot();
            let after_frame = renderer.render(&final_snapshot, page).await?;
            let mut after = after_frame.readability();
            source_duplicates(&final_snapshot, &mut after)?;
            after.sample_background(&background);
            let mut protected = before.clone();
            protected
                .layers
                .retain(|layer| !batch.removed_duplicates.contains(&layer.element));
            for group in &batch.balanced_groups {
                balance_floor(&mut protected, group);
                let sizes = after
                    .layers
                    .iter()
                    .filter(|layer| group.contains(&layer.element))
                    .map(|layer| layer.source_font_px)
                    .collect::<Vec<_>>();
                ensure!(
                    sizes.len() == group.len()
                        && sizes.iter().all(|size| (*size - sizes[0]).abs() < 0.25),
                    "dialogue in a balanced balloon must use one fitted size"
                );
            }
            ensure!(
                protected.regressions(&after).is_empty(),
                "sample {sample} regressed"
            );
            for layer in &before.layers {
                let old = initial.text_layer(layer.element)?;
                if batch.removed_duplicates.contains(&layer.element) {
                    ensure!(
                        duplicates.iter().any(|group| group.element == layer.element
                            && group.automatic
                            && group
                                .survivors
                                .iter()
                                .all(|id| after.layers.iter().any(|layer| layer.element == *id))),
                        "removed OCR needs visible source-backed survivors"
                    );
                    ensure!(
                        final_snapshot.text_content(old.content()?.id()).is_ok(),
                        "removing a rendered duplicate must retain its OCR provenance"
                    );
                    continue;
                }
                let new = final_snapshot.text_layer(layer.element)?;
                let restored = duplicates
                    .iter()
                    .filter_map(|group| group.restore.as_ref())
                    .find(|restore| restore.element == layer.element);
                let mut expected = old.content()?.translation()?;
                if let Some(restore) = restored
                    && let Some(translation) = &mut expected
                {
                    translation.text = koharu_scene::Authored::user(format!(
                        "{} {}",
                        restore.word,
                        translation.text.value.trim_start()
                    ));
                }
                ensure!(expected == new.content()?.translation()?, "wording changed");
                ensure!(old.frame()? == new.frame()?, "bubble geometry changed");
                let mode = |frame: &Frame| match frame.layer(layer.element).map(|l| l.kind()) {
                    Some(LayerKind::Text(text)) => Some(text.writing_mode),
                    _ => None,
                };
                ensure!(
                    mode(&before_frame) == mode(&after_frame),
                    "the rendered writing direction changed for {}",
                    layer.element
                );
                if let (Some(a), Some(b)) = (old.typography()?, new.typography()?) {
                    ensure!(
                        a.preferred_font == b.preferred_font
                            && a.font_weight == b.font_weight
                            && a.font_style == b.font_style
                            && a.alignment == b.alignment
                            && (!matches!(a.origin, koharu_scene::Origin::User)
                                || a.writing_mode == b.writing_mode),
                        "unrequested formatting changed"
                    );
                }
            }
            let original_pixels = crate::commands::output::frame_image(
                rasterizer.clone(),
                &after_frame.without_text()?,
            )
            .await?;
            ensure!(
                background == original_pixels,
                "artwork/cleanup pixels changed"
            );
            for (phase, frame) in [("before", before_frame), ("after", after_frame)] {
                for width in [390, 960] {
                    let bytes = crate::commands::output::frame_preview(
                        rasterizer.clone(),
                        &frame,
                        None,
                        Some(width),
                    )
                    .await?;
                    std::fs::write(
                        output.join(format!("{:02}-{phase}-{width}.webp", sample + 1)),
                        bytes,
                    )?;
                }
            }
            evidence.push(serde_json::json!({"location":super::super::context::location(&final_snapshot,page)?,"changes":changes,"attempted_layers":batch.elements,"ocr_duplicates":duplicates,"removed_duplicates":batch.removed_duplicates,"balanced_groups":batch.balanced_groups,"rejected_candidates":batch.declined,"before":before,"after":after}));
        }
        std::fs::write(
            output.join("native-readability.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
        println!(
            "{} detached manga pages validated with OCR reconciliation and balanced lettering. Surviving dialogue and unchanged artwork are checked. Remaining findings are recorded for review.",
            evidence.len()
        );
        Ok(())
    }
}
