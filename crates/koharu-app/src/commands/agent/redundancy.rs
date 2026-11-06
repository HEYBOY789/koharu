//! Original OCR regions identify repeated detections even after text is reflowed.
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use koharu_renderer::{ReadabilityIssue, ReadabilityKind, ReadabilityReport};
use koharu_scene::{BubbleRegion, EntityId, RegionSpec as _, Snapshot};
use serde::Serialize;

#[derive(Serialize)]
pub(super) struct RedundantText {
    pub element: EntityId,
    pub survivors: Vec<EntityId>,
    pub automatic: bool,
}

pub(super) struct OriginalText(Vec<Source>);

struct Source {
    element: EntityId,
    balloon: Option<EntityId>,
    bounds: [f64; 4],
    source: String,
    words: Vec<String>,
    translation: Vec<String>,
    translated_words: Vec<String>,
}

impl Source {
    fn area(&self) -> f64 {
        (self.bounds[2] - self.bounds[0]) * (self.bounds[3] - self.bounds[1])
    }

    fn overlap(&self, other: &Self, tolerance: f64) -> f64 {
        let [l, t, r, b] = self.bounds;
        let [ol, ot, or, ob] = other.bounds;
        let intersection = ((r + tolerance).min(or) - (l - tolerance).max(ol)).max(0.0)
            * ((b + tolerance).min(ob) - (t - tolerance).max(ot)).max(0.0);
        intersection / self.area().min(other.area())
    }

    fn enough_source_text(&self) -> bool {
        self.source.chars().filter(|c| c.is_alphanumeric()).count() >= 12
    }
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

impl OriginalText {
    pub(super) fn read(snapshot: &Snapshot, report: &ReadabilityReport) -> Result<Self> {
        let mut sources = vec![];
        for layer in &report.layers {
            let text = snapshot.text_layer(layer.element)?;
            let content = text.content()?;
            let (Some(source), Some(region)) = (content.source()?, content.source_region()?) else {
                continue;
            };
            let geometry = region.geometry()?;
            if geometry.points.len() < 3 {
                continue;
            }
            let bounds = geometry.points.iter().fold(
                [
                    f64::INFINITY,
                    f64::INFINITY,
                    f64::NEG_INFINITY,
                    f64::NEG_INFINITY,
                ],
                |b, p| [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)],
            );
            if bounds.iter().any(|v| !v.is_finite())
                || bounds[2] <= bounds[0]
                || bounds[3] <= bounds[1]
            {
                continue;
            }
            let balloon = match text.automatic_target()? {
                Some(target) if target.region()?.kind == BubbleRegion::kind() => Some(target.id()),
                _ => None,
            };
            sources.push(Source {
                element: layer.element,
                balloon,
                bounds,
                words: words(&source.text.value),
                source: source
                    .text
                    .value
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase(),
                translation: layer.text.split_whitespace().map(str::to_owned).collect(),
                translated_words: words(&layer.text),
            });
        }
        Ok(Self(sources))
    }

    pub(super) fn audit(&self, report: &mut ReadabilityReport, groups: &[RedundantText]) {
        for (index, left) in self.0.iter().enumerate() {
            if left.balloon.is_none() || !left.enough_source_text() {
                continue;
            }
            for right in &self.0[index + 1..] {
                if left.balloon != right.balloon || left.source != right.source {
                    continue;
                }
                let overlap = left.overlap(right, 0.0);
                if overlap >= 0.8
                    && !report.issues.iter().any(|issue| {
                        issue.kind == ReadabilityKind::DuplicateCandidate
                            && ((issue.element == left.element
                                && issue.related == Some(right.element))
                                || (issue.element == right.element
                                    && issue.related == Some(left.element)))
                    })
                {
                    report.issues.push(ReadabilityIssue {
                        id: format!("source_duplicate:{}:{}", left.element, right.element),
                        element: left.element,
                        related: Some(right.element),
                        kind: ReadabilityKind::DuplicateCandidate,
                        value: overlap as f32,
                    });
                }
            }
        }
        for group in groups {
            let related = match group.survivors.as_slice() {
                [only] => Some(*only),
                _ => None,
            };
            if related.is_some_and(|other| {
                report.issues.iter().any(|issue| {
                    issue.kind == ReadabilityKind::DuplicateCandidate
                        && ((issue.element == group.element && issue.related == Some(other))
                            || (issue.element == other && issue.related == Some(group.element)))
                })
            }) {
                continue;
            }
            report.issues.push(ReadabilityIssue {
                id: format!("source_coverage:{}", group.element),
                element: group.element,
                related,
                kind: ReadabilityKind::DuplicateCandidate,
                value: 1.0,
            });
        }
    }

    pub(super) fn groups(&self) -> Vec<RedundantText> {
        // Prefer separate detections to a merged box. Reserve their evidence so
        // that a second candidate cannot discard any of the surviving text.
        let mut order = self.0.iter().enumerate().collect::<Vec<_>>();
        order.sort_by(|(a, left), (b, right)| right.area().total_cmp(&left.area()).then(b.cmp(a)));
        let mut reserved = HashSet::new();
        let mut redundant = HashSet::new();
        let mut groups = vec![];
        for (index, source) in &order {
            if reserved.contains(&source.element) || !source.enough_source_text() {
                continue;
            }
            let candidates = order
                .iter()
                .copied()
                .filter(|(_, other)| {
                    other.element != source.element
                    && !redundant.contains(&other.element)
                    && other.balloon == source.balloon
                    // OCR bounding boxes may differ by a few edge pixels.
                    && source.overlap(other, 2.0) >= 0.8
                })
                .collect::<Vec<_>>();
            let (survivors, automatic) = if let Some((_, other)) =
                candidates.iter().rev().find(|(other_index, other)| {
                    if source.source == other.source && source.translation == other.translation {
                        (source.area(), *index) > (other.area(), *other_index)
                    } else {
                        other.area() > source.area()
                            && source.source.ends_with(['.', '!', '?'])
                            && other
                                .source
                                .strip_prefix(&source.source)
                                .is_some_and(|tail| tail.starts_with(' '))
                            && source
                                .translation
                                .last()
                                .is_some_and(|last| last.ends_with(['.', '!', '?']))
                            && other.translation.starts_with(&source.translation)
                    }
                }) {
                (vec![other.element], true)
            } else {
                let parts = merged_parts(source, &candidates);
                if parts.is_empty() {
                    continue;
                }
                (
                    parts.iter().map(|part| part.element).collect(),
                    preserves_translation(source, parts),
                )
            };
            redundant.insert(source.element);
            reserved.extend(survivors.iter().copied());
            groups.push(RedundantText {
                element: source.element,
                survivors,
                automatic,
            });
        }
        groups
    }
}

fn contains(haystack: &[String], needle: &[String]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn shares_phrase(text: &[String], part: &[String]) -> bool {
    !part.is_empty()
        && part
            .windows(part.len().min(3))
            .any(|phrase| contains(text, phrase))
}

fn merged_parts<'a>(source: &Source, candidates: &[(usize, &'a Source)]) -> Vec<&'a Source> {
    let mut parts: Vec<&Source> = vec![];
    for &(_, part) in candidates {
        if part.area() < source.area()
            && shares_phrase(&source.words, &part.words)
            && shares_phrase(&source.translated_words, &part.translated_words)
            && parts.iter().all(|other| part.overlap(other, 0.0) < 0.2)
        {
            parts.push(part);
        }
    }
    if parts.len() < 2 {
        return vec![];
    }
    // OCR can reorder columns in a merged box, but each word occurrence must
    // be represented across the distinct source boxes.
    let mut occurrences = HashMap::new();
    for word in parts.iter().flat_map(|part| &part.words) {
        *occurrences.entry(word).or_insert(0usize) += 1;
    }
    for word in &source.words {
        match occurrences.get_mut(word) {
            Some(count) if *count > 0 => *count -= 1,
            _ => return vec![],
        }
    }
    parts
}

fn preserves_translation(source: &Source, mut parts: Vec<&Source>) -> bool {
    // Automatic removal is stricter than an audit candidate. Preserve order,
    // using the order of the original OCR as well as the translated text.
    let mut original = source.words.as_slice();
    let mut remaining = source.translation.as_slice();
    while !original.is_empty() && !remaining.is_empty() {
        let Some((index, _)) = parts
            .iter()
            .enumerate()
            .filter(|(_, part)| {
                original.starts_with(&part.words) && remaining.starts_with(&part.translation)
            })
            .max_by_key(|(_, part)| part.translation.len())
        else {
            return false;
        };
        let part = parts.remove(index);
        original = &original[part.words.len()..];
        remaining = &remaining[part.translation.len()..];
    }
    original.is_empty() && remaining.is_empty()
}
