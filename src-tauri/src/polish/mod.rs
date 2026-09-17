//! 全书润色：冻结草稿快照，按批次并行润色，最后按顺序写回章节。
//!
//! 一轮润色对应一个 `polish.json`：记录输入摘要（草稿与术语的哈希）、
//! 批次状态和失败信息。中断后只要输入未变化，就可以继续未完成的批次。

mod plan;
mod run;
mod store;

#[cfg(test)]
mod tests;

pub use plan::plan_batches;
pub use run::run_round;
pub use store::{invalidate_round, pending_segment_count, read_summary};

use crate::model::{Chapter, ItemStatus};
use crate::terms::Term;
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 轮次文件名。
pub const ROUND_FILE: &str = "polish.json";

/// 单个润色批次的状态。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PolishBatchStatus {
    /// 等待润色。
    Pending,
    /// 正在润色。
    Running,
    /// 润色成功。
    Succeeded,
    /// 润色失败，可重试。
    Failed,
}

/// 一个润色批次，按章节和字符预算切分。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolishBatch {
    pub id: String,
    pub chapter_id: String,
    pub segment_ids: Vec<String>,
    pub status: PolishBatchStatus,
    #[serde(default)]
    pub attempts: usize,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub elapsed_ms: Option<u64>,
}

/// 一轮润色固定的输入快照。
///
/// 草稿本身仍保存在章节文件（`target_before_polish`）中，`input_digest` 用于
/// 标记草稿内容；若中断期间发生了重译，旧轮次会被丢弃并重新开始。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolishRound {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub input_digest: String,
    pub terms_digest: String,
    pub terms: Vec<Term>,
    pub style_guide: Vec<String>,
    pub book_synopsis: Option<String>,
    #[serde(default)]
    pub chapter_digests: BTreeMap<String, Option<String>>,
    pub batches: Vec<PolishBatch>,
    #[serde(default)]
    pub finished: bool,
    #[serde(default)]
    pub last_error: Option<String>,
}

/// 润色轮次的汇总信息，用于界面展示。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PolishSummary {
    pub round_id: String,
    pub finished: bool,
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub pending: usize,
    #[serde(default)]
    pub pending_segments: usize,
    pub last_error: Option<String>,
    pub updated_at: String,
}

impl PolishRound {
    pub fn summary(&self) -> PolishSummary {
        let succeeded = self
            .batches
            .iter()
            .filter(|batch| batch.status == PolishBatchStatus::Succeeded)
            .count();
        let failed = self
            .batches
            .iter()
            .filter(|batch| batch.status == PolishBatchStatus::Failed)
            .count();
        let pending = self
            .batches
            .iter()
            .filter(|batch| {
                matches!(
                    batch.status,
                    PolishBatchStatus::Pending | PolishBatchStatus::Running
                )
            })
            .count();
        PolishSummary {
            round_id: self.id.clone(),
            finished: self.finished,
            total: self.batches.len(),
            succeeded,
            failed,
            pending,
            pending_segments: 0,
            last_error: self.last_error.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}

/// 规划阶段产出的批次描述，尚未写入轮次文件。
#[derive(Debug, Clone)]
pub struct PlannedBatch {
    pub id: String,
    pub chapter_id: String,
    pub segment_ids: Vec<String>,
}

/// 全书是否已具备润色条件：每章都有译文标题，且所有段落都已翻译。
pub fn book_translation_complete(chapters: &[Chapter]) -> bool {
    chapters.iter().all(|chapter| {
        chapter.target_title.is_some()
            && chapter
                .segments
                .iter()
                .all(|segment| segment.status == ItemStatus::Translated && segment.target.is_some())
    })
}

/// 当前 UTC 时间，RFC 3339 毫秒精度。
fn now() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// 截断过长的错误信息，保留结尾省略号。
fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}
