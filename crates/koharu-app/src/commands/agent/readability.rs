//! Per-turn coverage and visual verification. Tool-call counts are not quality checks.
use std::collections::HashMap;

use anyhow::{Context as _, Result, ensure};
use koharu_renderer::{ReadabilityKind, ReadabilityReport};
use koharu_scene::{BlobId, EntityId, Snapshot};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::commands::project::Project;

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
    let background =
        crate::commands::output::frame_image(rasterizer, &frame.without_text()?).await?;
    report.sample_background(&background);
    let regressions = before.regressions(&report);
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

pub(super) struct RepairBatch {
    pub patch: koharu_scene::Patch,
    pub report: ReadabilityReport,
    pub declined: Vec<String>,
}

/// Search the existing layout, not arbitrary rectangles across the artwork.
/// The final patch uses the same scene builders as Properties and is still
/// checked before it can cross the native commit seam.
pub(super) async fn repair(
    renderer: &koharu_renderer::Renderer,
    rasterizer: std::sync::Arc<koharu_rasterizer::Rasterizer>,
    snapshot: &Snapshot,
    element: EntityId,
    before: &ReadabilityReport,
) -> Result<(koharu_scene::Patch, ReadabilityReport)> {
    use crate::commands::{editing::TypographyUpdate, project::Typography};
    let layer = before
        .layers
        .iter()
        .find(|l| l.element == element)
        .context("target must be visible translated text")?;
    let mut typography: Typography =
        if let Some(value) = snapshot.text_layer(element)?.typography()? {
            serde_json::from_value(serde_json::to_value(value)?)?
        } else {
            serde_json::from_value(serde_json::json!({"auto_fit":true}))?
        };
    if let Some(bg) = &layer.background {
        let thick = layer.stroke_width / layer.source_font_px.max(1.0) > 0.08;
        if bg.light_fraction > 0.95 || bg.dark_fraction > 0.95 {
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
    let base = typography.clone();
    let desired = before.minimum_font_px * before.page_width as f32
        / koharu_renderer::READER_WIDTHS[0]
            .min(before.page_width)
            .max(1) as f32;
    let mut low = layer.source_font_px;
    let mut high = desired.max(low);
    let mut best = typography;
    for step in 0..7 {
        if high - low < 0.25 {
            break;
        }
        let size = if step == 0 { high } else { (low + high) * 0.5 };
        let mut trial = base.clone();
        trial.size = Some(size);
        trial.auto_fit = false;
        let patch = Project::typography_patch(
            snapshot,
            vec![TypographyUpdate {
                layer: element,
                typography: trial.clone(),
            }],
        )?;
        let candidate = snapshot.preview([&patch])?;
        let report = renderer
            .render(&candidate, before.page)
            .await?
            .readability();
        let new_overlap = report.issues.iter().any(|i| {
            matches!(
                i.kind,
                ReadabilityKind::OverlapCandidate | ReadabilityKind::DuplicateCandidate
            ) && (i.element == element || i.related == Some(element))
                && before
                    .issues
                    .iter()
                    .find(|old| old.id == i.id)
                    .is_none_or(|old| i.value > old.value + 0.01)
        });
        if before.regressions(&report).is_empty()
            && !new_overlap
            && !report
                .issues
                .iter()
                .any(|i| i.element == element && i.kind == ReadabilityKind::Overflow)
        {
            best = trial;
            low = size;
            if size >= desired {
                break;
            }
        } else {
            high = size;
        }
    }
    let patch = Project::typography_patch(
        snapshot,
        vec![TypographyUpdate {
            layer: element,
            typography: best,
        }],
    )?;
    let report =
        checked_candidate(renderer, rasterizer, snapshot, &patch, before.page, before).await?;
    Ok((patch, report))
}

pub(super) async fn repair_batch(
    renderer: &koharu_renderer::Renderer,
    rasterizer: std::sync::Arc<koharu_rasterizer::Rasterizer>,
    snapshot: &Snapshot,
    elements: &[EntityId],
    before: &ReadabilityReport,
) -> Result<RepairBatch> {
    ensure!(
        (1..=50).contains(&elements.len()),
        "Choose 1–50 audited text layers on one page"
    );
    let mut preview = snapshot.clone();
    let background = crate::commands::output::frame_image(
        rasterizer.clone(),
        &renderer
            .render(snapshot, before.page)
            .await?
            .without_text()?,
    )
    .await?;
    let mut updates = vec![];
    let mut seen = std::collections::HashSet::new();
    let mut declined = vec![];
    for element in elements {
        ensure!(seen.insert(*element), "Duplicate element ID");
        ensure!(
            super::scope::page_for(snapshot, *element)? == before.page,
            "Batch layers must belong to the requested page"
        );
        let mut current = renderer.render(&preview, before.page).await?.readability();
        current.sample_background(&background);
        let (patch, _) =
            match repair(renderer, rasterizer.clone(), &preview, *element, &current).await {
                Ok(result) => result,
                Err(error) if error.is::<CandidateRegression>() => {
                    declined.push(format!("{element}: {error}"));
                    continue;
                }
                Err(error) => return Err(error),
            };
        preview = preview.preview([&patch])?;
        let typography = preview
            .text_layer(*element)?
            .typography()?
            .context("repaired typography")?;
        updates.push(crate::commands::editing::TypographyUpdate {
            layer: *element,
            typography: serde_json::from_value(serde_json::to_value(typography)?)?,
        });
    }
    let patch = Project::typography_patch(snapshot, updates)?;
    let report =
        checked_candidate(renderer, rasterizer, snapshot, &patch, before.page, before).await?;
    Ok(RepairBatch {
        patch,
        report,
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
    pub page: String,
    /// One decision for EVERY remaining issue ID returned by audit_readability.
    /// Never label small text acceptable merely because it fits its box.
    pub issues: Vec<Decision>,
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
        let audit = self
            .pages
            .get(&page)
            .context("Call audit_readability for this page before editing it")?;
        ensure!(
            audit.stamp == stamp,
            "Page changed. Call audit_readability again before editing."
        );
        ensure!(
            audit.viewed,
            "Call view_page before editing this page; inspect the text and its background."
        );
        Ok(Some(audit.report.clone()))
    }

    pub fn changed(&mut self, page: EntityId) {
        self.pages.remove(&page);
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
        let audit = self
            .pages
            .get_mut(&page)
            .context("Call audit_readability after the latest edit")?;
        ensure!(
            audit.stamp == stamp,
            "Page changed; audit and view it again."
        );
        ensure!(
            audit.viewed && audit.phone && audit.desktop,
            "View the current page at BOTH phone (390px) and desktop (960px) widths before verifying. view_page without crop/display_width supplies both during review."
        );
        ensure!(
            decisions.len() == audit.report.issues.len(),
            "Supply exactly one decision per remaining issue, or fix it and audit again."
        );
        let mut seen = std::collections::HashSet::new();
        for decision in &decisions {
            let issue = audit
                .report
                .issues
                .iter()
                .find(|i| i.id == decision.issue)
                .context("Unknown issue ID; use the current audit")?;
            ensure!(seen.insert(&decision.issue), "Duplicate issue decision");
            ensure!(
                (12..=1000).contains(&decision.reason.trim().len()),
                "Give a concrete reason (12–1000 bytes)"
            );
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
        assert!(review.verify(page, stamp, vec![decision]).is_err());
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
        let pages = snapshot.chapters()?.entries[0].pages[15..17].to_vec();
        let renderer = koharu_renderer::Renderer::new()?;
        let rasterizer = std::sync::Arc::new(koharu_rasterizer::Rasterizer::new()?);
        let mut reports = vec![];
        for (index, page) in pages.into_iter().enumerate() {
            let frame = renderer.render(&snapshot, page).await?;
            for width in [390, 960] {
                let bytes = crate::commands::output::frame_preview(
                    rasterizer.clone(),
                    &frame,
                    None,
                    Some(width),
                )
                .await?;
                std::fs::write(
                    output.join(format!("page-{}-{width}.webp", index + 16)),
                    bytes,
                )?;
            }
            reports.push(frame.readability());
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
        let requested = [(0, 0), (0, 15), (1, 1), (1, 3), (2, 0)];
        let renderer = koharu_renderer::Renderer::new()?;
        let rasterizer = Arc::new(koharu_rasterizer::Rasterizer::new()?);
        let mut evidence = vec![];
        for (sample, (chapter, index)) in requested.into_iter().enumerate() {
            let page = *chapters
                .entries
                .get(chapter)
                .context("chapter")?
                .pages
                .get(index)
                .context("page")?;
            let initial = project.snapshot();
            let before_frame = renderer.render(&initial, page).await?;
            let background = crate::commands::output::frame_image(
                rasterizer.clone(),
                &before_frame.without_text()?,
            )
            .await?;
            let mut before = before_frame.readability();
            before.sample_background(&background);
            let mut failures = vec![];
            let mut changes = 0;
            let targets = before
                .layers
                .iter()
                .filter(|l| {
                    before.issues.iter().any(|i| {
                        i.element == l.element
                            && matches!(
                                i.kind,
                                ReadabilityKind::SmallText
                                    | ReadabilityKind::ThickOutline
                                    | ReadabilityKind::LowContrast
                            )
                    })
                })
                .map(|l| l.element)
                .collect::<Vec<_>>();
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
            if !targets.is_empty() {
                let batch = repair_batch(
                    &renderer,
                    rasterizer.clone(),
                    &project.snapshot(),
                    &targets,
                    &before,
                )
                .await?;
                failures = batch.declined;
                let commit = project.commit(batch.patch).await?;
                if commit.changes.from != commit.changes.to {
                    changes += 1;
                    project.record_commit(&commit);
                }
            }
            let final_snapshot = project.snapshot();
            let after_frame = renderer.render(&final_snapshot, page).await?;
            let mut after = after_frame.readability();
            after.sample_background(&background);
            ensure!(
                before.regressions(&after).is_empty(),
                "sample {sample} regressed"
            );
            for layer in &before.layers {
                let old = initial.text_layer(layer.element)?;
                let new = final_snapshot.text_layer(layer.element)?;
                ensure!(
                    old.content()?.translation()? == new.content()?.translation()?,
                    "wording changed"
                );
                ensure!(old.frame()? == new.frame()?, "bubble geometry changed");
                if let (Some(a), Some(b)) = (old.typography()?, new.typography()?) {
                    ensure!(
                        a.preferred_font == b.preferred_font
                            && a.font_weight == b.font_weight
                            && a.font_style == b.font_style
                            && a.alignment == b.alignment
                            && a.writing_mode == b.writing_mode,
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
            evidence.push(serde_json::json!({"location":super::super::context::location(&final_snapshot,page)?,"changes":changes,"rejected_candidates":failures,"before":before,"after":after}));
        }
        std::fs::write(
            output.join("native-readability.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )?;
        println!(
            "Five detached manga pages validated; no words, bubble geometry, font faces or artwork changed. Remaining findings are recorded for review."
        );
        Ok(())
    }
}
