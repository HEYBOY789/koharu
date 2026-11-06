//! Original OCR regions identify repeated detections even after text is reflowed.
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use koharu_renderer::{ReadabilityIssue, ReadabilityKind, ReadabilityReport};
use koharu_scene::{
    Authored, BubbleRegion, Edit, EntityId, LanguageTag, Origin, RegionSpec as _, RemovePolicy,
    Snapshot,
};
use serde::Serialize;

#[derive(Serialize)]
pub(super) struct RedundantText {
    pub element: EntityId,
    pub survivors: Vec<EntityId>,
    pub automatic: bool,
    pub requires_source_review: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore: Option<LeadingWord>,
}

#[derive(Serialize)]
pub(super) struct LeadingWord {
    pub element: EntityId,
    pub word: String,
}

impl RedundantText {
    pub(super) fn apply(&self, snapshot: &Snapshot, edit: &mut Edit) -> koharu_scene::Result<()> {
        if let Some(restore) = &self.restore {
            let layer = snapshot.text_layer(restore.element)?;
            let content = layer.content()?;
            let mut translation = content.translation()?.ok_or_else(|| {
                koharu_scene::Error::Invalid("missing surviving translation".into())
            })?;
            translation.text = Authored::user(format!(
                "{} {}",
                restore.word,
                translation.text.value.trim_start()
            ));
            edit.promote_entity_to_user(layer.id())?;
            edit.promote_entity_to_user(content.id())?;
            edit.set(content.id(), &translation)?;
        }
        edit.remove_entity(self.element, RemovePolicy::Cascade)
    }
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
    language: Option<LanguageTag>,
    generated_translation: bool,
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
            let translation = content.translation()?;
            sources.push(Source {
                element: layer.element,
                balloon,
                bounds,
                words: words(&source.text.value),
                source: source
                    .text
                    .value
                    .lines()
                    .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
                    .collect::<Vec<_>>()
                    .join("\n")
                    .to_lowercase(),
                translation: layer.text.split_whitespace().map(str::to_owned).collect(),
                translated_words: words(&layer.text),
                generated_translation: translation
                    .as_ref()
                    .is_some_and(|value| matches!(value.text.origin, Origin::Generated(_))),
                language: translation.and_then(|value| value.language),
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
        let mut groups: Vec<RedundantText> = vec![];
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
            let (survivors, mut automatic, restore, requires_source_review) =
                if let Some((_, other)) = candidates.iter().rev().find(|(other_index, other)| {
                    if source.source == other.source && source.translation == other.translation {
                        (source.area(), *index) > (other.area(), *other_index)
                    } else {
                        other.area() > source.area()
                            && (other
                                .source
                                .strip_prefix(&source.source)
                                .is_some_and(|tail| tail.starts_with('\n'))
                                || (source.source.ends_with(['.', '!', '?'])
                                    && other
                                        .source
                                        .strip_prefix(&source.source)
                                        .is_some_and(|tail| tail.starts_with(' '))
                                    && source
                                        .translation
                                        .last()
                                        .is_some_and(|last| last.ends_with(['.', '!', '?']))))
                            && other.translation.starts_with(&source.translation)
                    }
                }) {
                    (vec![other.element], true, None, false)
                } else if let Some((_, other)) = candidates.iter().find(|(_, other)| {
                    other.area() > source.area() && conflicting_clocks(source, other)
                }) {
                    (vec![other.element], false, None, true)
                } else {
                    let parts = merged_parts(source, &candidates);
                    if parts.is_empty() {
                        let parts = translated_parts(source, &candidates);
                        if parts.is_empty() {
                            continue;
                        }
                        (
                            parts.iter().map(|part| part.element).collect(),
                            true,
                            None,
                            false,
                        )
                    } else {
                        let survivors = parts.iter().map(|part| part.element).collect();
                        if preserves_translation(source, parts.clone()) {
                            (survivors, true, None, false)
                        } else {
                            let repair = reconcile_pair(source, &parts);
                            (survivors, repair.is_some(), repair.flatten(), false)
                        }
                    }
                };
            // Two overlapping merged detections can disagree on a leading
            // word. Keep that second candidate for review rather than
            // overwriting the first restoration in the same scene patch.
            if let Some(restore) = &restore
                && groups.iter().any(|group| {
                    group.automatic
                        && group.restore.as_ref().is_some_and(|earlier| {
                            earlier.element == restore.element && earlier.word != restore.word
                        })
                })
            {
                automatic = false;
            }
            redundant.insert(source.element);
            reserved.extend(survivors.iter().copied());
            groups.push(RedundantText {
                element: source.element,
                survivors,
                automatic,
                requires_source_review,
                restore,
            });
        }
        groups
    }
}

/// A merged OCR box can scramble columns even when its generated translation
/// repeats the complete translations of disjoint detections. Every normalized
/// word occurrence must survive, including negation and numbers.
fn translated_parts<'a>(source: &Source, candidates: &[(usize, &'a Source)]) -> Vec<&'a Source> {
    if !source.generated_translation || source.language.is_none() {
        return vec![];
    }
    let mut remaining = source.translated_words.as_slice();
    let mut parts: Vec<&Source> = vec![];
    while !remaining.is_empty() {
        let Some((_, part)) = candidates
            .iter()
            .filter(|(_, part)| {
                part.generated_translation
                    && part.language == source.language
                    && part.area() < source.area()
                    && part.translated_words.len() >= 3
                    && !parts.iter().any(|other| {
                        other.element == part.element || part.overlap(other, 0.0) >= 0.2
                    })
                    && remaining.starts_with(&part.translated_words)
            })
            .max_by_key(|(_, part)| part.translated_words.len())
        else {
            return vec![];
        };
        remaining = &remaining[part.translated_words.len()..];
        parts.push(*part);
    }
    if parts.len() >= 2 { parts } else { vec![] }
}

/// Conflicting clock digits are only a visual-review candidate. Never choose
/// one reading automatically or use this rule for ordinary dialogue.
fn conflicting_clocks(fragment: &Source, aggregate: &Source) -> bool {
    let numeric = |word: &String| !word.is_empty() && word.chars().all(|c| c.is_ascii_digit());
    if fragment.source.matches(':').count() < 2
        || fragment.words.len() < 6
        || !fragment.words.iter().all(numeric)
        || fragment.words != fragment.translated_words
    {
        return false;
    }
    let numbers = aggregate
        .words
        .iter()
        .filter(|word| numeric(word))
        .collect::<Vec<_>>();
    let translated = aggregate
        .translated_words
        .iter()
        .filter(|word| numeric(word))
        .collect::<Vec<_>>();
    numbers == translated
        && numbers.len() == fragment.words.len()
        && fragment
            .words
            .iter()
            .zip(numbers)
            .filter(|(left, right)| left != right)
            .count()
            == 1
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

enum Fragment<'a> {
    Same,
    Plural,
    Restore(&'a str),
}

/// Reconcile one wording difference in a proven pair of OCR fragments. A
/// unique sentence boundary prevents moving a word to the wrong speech.
fn reconcile_pair(source: &Source, parts: &[&Source]) -> Option<Option<LeadingWord>> {
    let &[first, second] = parts else {
        return None;
    };
    let language = source.language.as_ref()?;
    if parts
        .iter()
        .any(|part| part.language.as_ref() != Some(language))
        || source.translation.len() > 256
    {
        return None;
    }
    let vietnamese = language
        .as_str()
        .split('-')
        .next()
        .is_some_and(|tag| tag.eq_ignore_ascii_case("vi"));
    let mut repair = None;
    for (left, right) in [(first, second), (second, first)] {
        for split in 1..source.translation.len() {
            let (opening, ending) = source.translation.split_at(split);
            if !opening
                .last()
                .is_some_and(|word| word.ends_with(['.', '!', '?']))
            {
                continue;
            }
            let candidate = match (
                match_fragment(opening, left, vietnamese),
                match_fragment(ending, right, vietnamese),
            ) {
                (Some(Fragment::Restore(word)), Some(Fragment::Same)) => Some(LeadingWord {
                    element: left.element,
                    word: word.to_owned(),
                }),
                (Some(Fragment::Same), Some(Fragment::Restore(word))) => Some(LeadingWord {
                    element: right.element,
                    word: word.to_owned(),
                }),
                (Some(Fragment::Plural), Some(Fragment::Same))
                | (Some(Fragment::Same), Some(Fragment::Plural)) => None,
                _ => continue,
            };
            if repair.is_some() {
                return None;
            }
            repair = Some(candidate);
        }
    }
    repair
}

fn match_fragment<'a>(
    fragment: &'a [String],
    part: &Source,
    vietnamese: bool,
) -> Option<Fragment<'a>> {
    let kept = part.translation.as_slice();
    if kept.starts_with(fragment)
        && fragment
            .last()
            .is_some_and(|word| word.ends_with(['.', '!', '?']))
    {
        return Some(Fragment::Same);
    }
    if part.generated_translation
        && let Some((word, rest)) = fragment.split_first()
        && rest == kept
        && word.chars().all(char::is_alphabetic)
    {
        return Some(Fragment::Restore(word));
    }
    // A split Vietnamese line can already spell out a plural in the same
    // source. Preserve that complete line and every other word as written.
    if vietnamese && kept.len() == fragment.len() + 1 {
        let extra = kept.iter().zip(fragment).position(|(a, b)| a != b)?;
        if kept[extra].eq_ignore_ascii_case("các") && kept[extra + 1..] == fragment[extra..] {
            return Some(Fragment::Plural);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detection(original: &str, translated: &str, bounds: [f64; 4]) -> Source {
        let mut value = source(translated, true);
        value.source = original.to_lowercase();
        value.words = words(original);
        value.bounds = bounds;
        value
    }

    #[test]
    fn garbled_merged_ocr_keeps_complete_generated_translation_fragments() {
        let merged = detection(
            "AND THERE THAT REALLY WHY WAS JLIST AT MOTHER CORE",
            "Sâu thẳm là tình yêu... và đó là lý do bà ấy không thể chỉ là mẹ.",
            [0.0, 0.0, 240.0, 160.0],
        );
        let opening = detection(
            "There really was love at the core...",
            "Sâu thẳm là tình yêu...",
            [0.0, 0.0, 100.0, 160.0],
        );
        let ending = detection(
            "...And that's why she couldn't stop at being a mother.",
            "...Và đó là lý do bà ấy không thể chỉ là mẹ.",
            [140.0, 0.0, 240.0, 160.0],
        );
        let merged_id = merged.element;
        let opening_id = opening.element;
        let ending_id = ending.element;
        let mut original = OriginalText(vec![merged, opening, ending]);
        let groups = original.groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].element, merged_id);
        assert!(groups[0].automatic);
        assert!(groups[0].survivors.contains(&opening_id));
        assert!(groups[0].survivors.contains(&ending_id));

        original.0[0].translated_words.insert(0, "không".into());
        assert!(original.groups().iter().all(|group| !group.automatic));
        original.0[0].translated_words.remove(0);
        original.0[1].generated_translation = false;
        assert!(original.groups().iter().all(|group| !group.automatic));
        original.0[1].generated_translation = true;
        original.0[2].bounds = [20.0, 0.0, 120.0, 160.0];
        assert!(original.groups().iter().all(|group| !group.automatic));
    }

    #[test]
    fn free_text_heading_requires_a_complete_source_line_and_preserved_translation() {
        let heading = detection("Midterm exams", "Kỳ thi giữa kỳ", [0.0, 0.0, 220.0, 30.0]);
        let aggregate = detection(
            "Midterm exams\nJapanese 9:50 - 16\nEnglish 10:55 -\nMath 13:10 -",
            "Kỳ thi giữa kỳ\nQuốc ngữ 9:50 - 16\nTiếng Anh 10:55 -\nToán 13:10 -",
            [0.0, 0.0, 240.0, 180.0],
        );
        let id = heading.element;
        let survivor = aggregate.element;
        let mut original = OriginalText(vec![heading, aggregate]);
        let groups = original.groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].element, id);
        assert_eq!(groups[0].survivors, vec![survivor]);
        assert!(groups[0].automatic);
        original.0[1].source = original.0[1].source.replace("exams\n", "exams postponed\n");
        assert!(original.groups().is_empty());
    }

    #[test]
    fn conflicting_clock_ocr_is_never_removed_automatically() {
        let clocks = detection(
            "9:50-14\n10:55-\n13:10-",
            "9:50-14\n10:55-\n13:10-",
            [130.0, 40.0, 220.0, 170.0],
        );
        let aggregate = detection(
            "Midterm exams\nJapanese 9:50 - 16\nEnglish 10:55 -\nMath 13:10 -",
            "Kỳ thi giữa kỳ\nQuốc ngữ 9:50 - 16\nTiếng Anh 10:55 -\nToán 13:10 -",
            [0.0, 0.0, 240.0, 180.0],
        );
        let id = clocks.element;
        let mut original = OriginalText(vec![clocks, aggregate]);
        let groups = original.groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].element, id);
        assert!(!groups[0].automatic);
        original.0[0].words[3] = "11".into();
        assert!(original.groups().is_empty());
    }

    fn source(text: &str, generated: bool) -> Source {
        Source {
            element: EntityId::new(),
            balloon: None,
            bounds: [0.0, 0.0, 100.0, 100.0],
            source: "overlapping OCR verified by the calling code".into(),
            words: vec![],
            translation: text.split_whitespace().map(str::to_owned).collect(),
            translated_words: words(text),
            language: Some(LanguageTag::new("vi-VN").unwrap()),
            generated_translation: generated,
        }
    }

    #[test]
    fn reconciliation_keeps_missing_words_and_declines_changed_meaning() {
        let merged = source("Tôi đã gọi rồi... Cảm ơn bạn!", true);
        let mut opening = source("đã gọi rồi...", true);
        let ending = source("Cảm ơn bạn!", true);
        let restore = reconcile_pair(&merged, &[&ending, &opening])
            .expect("all fragments have one unambiguous matching boundary")
            .expect("a unique leading word must move to its surviving fragment");
        assert_eq!(restore.element, opening.element);
        assert_eq!(restore.word, merged.translation[0]);
        opening.generated_translation = false;
        assert!(reconcile_pair(&merged, &[&ending, &opening]).is_none());

        let merged = source("Cảm ơn bạn! Ủng hộ tác giả và nhà xuất bản.", true);
        let explicit_plural = source("Ủng hộ tác giả và các nhà xuất bản.", true);
        assert!(matches!(
            reconcile_pair(&merged, &[&ending, &explicit_plural]),
            Some(None)
        ));
        let negated = source("Không ủng hộ tác giả và nhà xuất bản.", true);
        assert!(reconcile_pair(&merged, &[&ending, &negated]).is_none());

        let changed = source("Tôi đã gọi rồi... Cảm ơn bạn! Mang thuốc.", true);
        opening.generated_translation = true;
        assert!(reconcile_pair(&changed, &[&ending, &opening]).is_none());
    }

    #[test]
    fn conflicting_merged_detections_cannot_overwrite_a_restored_word() {
        let make = |original: &str, translated: &str, bounds| {
            let mut source = source(translated, true);
            source.source = original.to_lowercase();
            source.words = words(original);
            source.bounds = bounds;
            source
        };
        let first = make(
            "Already called. Thank you.",
            "Tôi đã gọi. Cảm ơn.",
            [0.0, 0.0, 200.0, 100.0],
        );
        let second = make(
            "Already called. Thank you.",
            "Ta đã gọi. Cảm ơn.",
            [0.0, 0.0, 200.0, 100.0],
        );
        let opening = make("Already called.", "đã gọi.", [0.0, 0.0, 90.0, 100.0]);
        let ending = make("Thank you.", "Cảm ơn.", [110.0, 0.0, 200.0, 100.0]);
        let groups = OriginalText(vec![first, second, opening, ending]).groups();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups.iter().filter(|group| group.automatic).count(), 1);
        assert!(groups.iter().all(|group| group.restore.is_some()));
    }
}
