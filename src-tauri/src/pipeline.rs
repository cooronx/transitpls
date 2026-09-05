use crate::llm::RecentTarget;
use crate::model::{Chapter, ItemStatus, Segment};
use crate::state;
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextFile {
    pub updated_at: String,
    pub recent_targets: Vec<RecentTarget>,
}

pub fn batch_ranges(segments: &[Segment], max_chars: usize) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < segments.len() {
        let mut end = start;
        let mut chars = 0;
        while end < segments.len() {
            let next = segments[end].source.chars().count();
            if end > start && chars + next > max_chars {
                break;
            }
            chars += next;
            end += 1;
            if chars >= max_chars {
                break;
            }
        }
        ranges.push(start..end);
        start = end;
    }
    ranges
}

pub fn recent_targets(
    chapters: &[Chapter],
    before_chapter: usize,
    before_segment: usize,
    max_chars: usize,
) -> Vec<RecentTarget> {
    let mut nearest_first = Vec::new();
    let mut remaining = max_chars;
    for chapter_index in (0..=before_chapter).rev() {
        let chapter = &chapters[chapter_index];
        let end = if chapter_index == before_chapter {
            before_segment.min(chapter.segments.len())
        } else {
            chapter.segments.len()
        };
        for segment in chapter.segments[..end].iter().rev() {
            if segment.status != ItemStatus::Translated {
                return chronological(nearest_first);
            }
            let Some(target) = segment.target.as_deref() else {
                return chronological(nearest_first);
            };
            if remaining == 0 {
                return chronological(nearest_first);
            }
            let target_chars = target.chars().count();
            let value = if target_chars > remaining {
                target
                    .chars()
                    .skip(target_chars - remaining)
                    .collect::<String>()
            } else {
                target.to_string()
            };
            remaining = remaining.saturating_sub(target_chars);
            nearest_first.push(RecentTarget {
                chapter_id: chapter.id.clone(),
                segment_id: segment.id.clone(),
                target: value,
            });
        }
    }
    chronological(nearest_first)
}

fn chronological(mut values: Vec<RecentTarget>) -> Vec<RecentTarget> {
    values.reverse();
    values
}

pub fn write_context(
    project_dir: &Path,
    chapters: &[Chapter],
    before_chapter: usize,
    before_segment: usize,
    max_chars: usize,
) -> Result<(), String> {
    state::write_json_atomic(
        &project_dir.join("context.json"),
        &ContextFile {
            updated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            recent_targets: recent_targets(chapters, before_chapter, before_segment, max_chars),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::{batch_ranges, recent_targets};
    use crate::model::{Chapter, ItemStatus, Segment, SegmentKind};

    fn segment(id: &str, source: &str, target: Option<&str>, status: ItemStatus) -> Segment {
        Segment {
            id: id.to_string(),
            ordinal: 0,
            source: source.to_string(),
            target: target.map(str::to_string),
            target_before_polish: None,
            kind: SegmentKind::Paragraph,
            status,
            source_hash: "hash".to_string(),
            meta: serde_json::json!({}),
        }
    }

    #[test]
    fn batches_by_unicode_source_characters_and_keeps_oversize_segment() {
        let segments = vec![
            segment("a", "你好", None, ItemStatus::Pending),
            segment("b", "世界", None, ItemStatus::Pending),
            segment("c", "超长文本", None, ItemStatus::Pending),
        ];
        assert_eq!(batch_ranges(&segments, 3), vec![0..1, 1..2, 2..3]);
    }

    #[test]
    fn recent_context_crosses_chapters_and_stops_at_a_gap() {
        let chapters = vec![
            Chapter {
                id: "one".to_string(),
                title: "One".to_string(),
                target_title: None,
                status: ItemStatus::Translated,
                meta: serde_json::json!({}),
                segments: vec![segment("a", "a", Some("甲乙"), ItemStatus::Translated)],
            },
            Chapter {
                id: "two".to_string(),
                title: "Two".to_string(),
                target_title: None,
                status: ItemStatus::Pending,
                meta: serde_json::json!({}),
                segments: vec![
                    segment("b", "b", Some("丙丁"), ItemStatus::Translated),
                    segment("c", "c", None, ItemStatus::Pending),
                ],
            },
        ];
        let context = recent_targets(&chapters, 1, 1, 3);
        assert_eq!(context.len(), 2);
        assert_eq!(context[0].target, "乙");
        assert_eq!(context[1].target, "丙丁");
        assert!(recent_targets(&chapters, 1, 2, 10).is_empty());
    }
}
