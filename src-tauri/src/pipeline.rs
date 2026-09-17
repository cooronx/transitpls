//! 翻译批次与上下文：按字符预算切分批次，并收集最近译文作为提示词参考。

use crate::llm::RecentTarget;
use crate::model::{Chapter, ItemStatus, Segment};
use crate::state;
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::ops::Range;
use std::path::Path;

/// 项目目录下 `context.json` 的内容，记录最近的译文上下文。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextFile {
    pub updated_at: String,
    pub recent_targets: Vec<RecentTarget>,
}

/// 按原文 Unicode 字符数把段落切成批次区间；单段超限时独占一个批次。
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

/// 收集指定位置之前最近的已翻译段落，按时间顺序返回。
pub fn recent_targets(
    chapters: &[Chapter],
    before_chapter: usize,
    before_segment: usize,
    max_chars: usize,
) -> Vec<RecentTarget> {
    collect_recent(
        chapters,
        before_chapter,
        before_segment,
        max_chars,
        |segment| segment.target.as_deref(),
    )
}

/// 润色轮次的参考上下文：始终读取冻结的草稿（`target_before_polish`），
/// 避免并行批次通过已润色的 `target` 互相影响。
pub fn recent_drafts(
    chapters: &[Chapter],
    before_chapter: usize,
    before_segment: usize,
    max_chars: usize,
) -> Vec<RecentTarget> {
    collect_recent(
        chapters,
        before_chapter,
        before_segment,
        max_chars,
        |segment| {
            segment
                .target_before_polish
                .as_deref()
                .or(segment.target.as_deref())
        },
    )
}

/// 从 `before_chapter`/`before_segment` 向前收集译文，直到超出字符预算、
/// 遇到未翻译段落或没有译文为止；结果按时间顺序返回。
fn collect_recent<F>(
    chapters: &[Chapter],
    before_chapter: usize,
    before_segment: usize,
    max_chars: usize,
    mut value_of: F,
) -> Vec<RecentTarget>
where
    F: FnMut(&Segment) -> Option<&str>,
{
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
            let Some(target) = value_of(segment) else {
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

/// 把最近上下文写入项目目录的 `context.json`。
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
            polish_status: None,
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
