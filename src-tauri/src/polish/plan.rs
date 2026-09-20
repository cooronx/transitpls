//! 批次规划与轮次快照摘要计算。

use super::PlannedBatch;
use crate::model::Chapter;
use crate::pipeline;
use crate::terms::Term;
use chrono::{SecondsFormat, Utc};
use sha2::{Digest, Sha256};

/// 按章节切分润色批次。
///
/// 只挑选有草稿且尚未润色成功的段落，再按 `max_chars` 预算组批；
/// 批次不跨章节，保证参考上下文与章节摘要对应。
pub fn plan_batches(round_id: &str, chapters: &[Chapter], max_chars: usize) -> Vec<PlannedBatch> {
    let mut batches = Vec::new();
    for chapter in chapters {
        let eligible = chapter
            .segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| crate::revisions::eligible_for_polish(segment))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        if eligible.is_empty() {
            continue;
        }
        let projected = eligible
            .iter()
            .map(|&index| chapter.segments[index].clone())
            .collect::<Vec<_>>();
        for range in pipeline::batch_ranges(&projected, max_chars) {
            let indices = &eligible[range];
            batches.push(PlannedBatch {
                id: format!("{round_id}-b{:03}", batches.len()),
                chapter_id: chapter.id.clone(),
                segment_ids: indices
                    .iter()
                    .map(|&index| chapter.segments[index].id.clone())
                    .collect(),
            });
        }
    }
    batches
}

/// 计算全部润色草稿的摘要，用于判断旧轮次是否仍然有效。
pub(super) fn drafts_digest(chapters: &[Chapter]) -> String {
    let mut hasher = Sha256::new();
    for chapter in chapters {
        for segment in &chapter.segments {
            if let Some(draft) = &segment.target_before_polish {
                hasher.update([u8::from(crate::revisions::is_protected(segment))]);
                hasher.update(segment.id.as_bytes());
                hasher.update([0x1f]);
                hasher.update(draft.as_bytes());
                hasher.update([0x1e]);
            }
        }
    }
    format_digest(hasher.finalize())
}

/// 计算术语库摘要；术语变化同样会使旧轮次失效。
pub(super) fn terms_digest(terms: &[Term]) -> String {
    let bytes = serde_json::to_vec(terms).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format_digest(hasher.finalize())
}

fn format_digest(hash: impl AsRef<[u8]>) -> String {
    hash.as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 生成轮次 ID：`round-<RFC 3339 毫秒时间>`。
pub(super) fn new_round_id() -> String {
    format!(
        "round-{}",
        Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
    )
}
