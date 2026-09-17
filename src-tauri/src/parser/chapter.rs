//! 章节构建：把文本块切分为段落并生成稳定的章节/段落 ID。

use super::text::{hash_text, normalize_source, split_long_text};
use crate::model::{Chapter, ItemStatus, Segment, SegmentKind};
use std::collections::HashMap;

pub(super) fn build_chapter(
    ordinal: usize,
    title: String,
    paragraphs: Vec<String>,
    max_chars: usize,
) -> Chapter {
    let blocks = paragraphs
        .into_iter()
        .map(|paragraph| (SegmentKind::Paragraph, paragraph))
        .collect();
    build_chapter_from_blocks(ordinal, title, blocks, max_chars)
}

/// 由带类型的文本块构建章节。
///
/// 章节 ID 为 `chapter-序号-标题哈希`；段落 ID 由章节 ID、类型和原文哈希生成，
/// 内容相同的段落会追加计数后缀以保证唯一。
pub(super) fn build_chapter_from_blocks(
    ordinal: usize,
    title: String,
    blocks: Vec<(SegmentKind, String)>,
    max_chars: usize,
) -> Chapter {
    let chapter_id = format!("chapter-{}-{}", ordinal + 1, hash_text(&title));
    let mut seen = HashMap::<String, usize>::new();
    let mut segments = Vec::new();
    let mut segment_ordinal = 0;
    for (kind, source) in blocks {
        for chunk in split_long_text(&source, max_chars) {
            let normalized = normalize_source(&chunk);
            if normalized.is_empty() {
                continue;
            }
            let base = format!(
                "seg-{}",
                hash_text(&format!("{}{:?}{}", chapter_id, kind, normalized))
            );
            let count = seen.entry(base.clone()).or_insert(0);
            *count += 1;
            let id = if *count == 1 {
                base
            } else {
                format!("{}-{}", base, count)
            };
            segments.push(Segment {
                id,
                ordinal: segment_ordinal,
                source: chunk.clone(),
                target: None,
                target_before_polish: None,
                polish_status: None,
                kind: kind.clone(),
                status: ItemStatus::Pending,
                source_hash: hash_text(&chunk),
                meta: serde_json::json!({}),
            });
            segment_ordinal += 1;
        }
    }
    Chapter {
        id: chapter_id,
        title,
        target_title: None,
        status: ItemStatus::Pending,
        meta: serde_json::json!({}),
        segments,
    }
}
