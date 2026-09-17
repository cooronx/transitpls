//! 轮次文件的读写与失效。

use super::{PolishRound, PolishSummary, ROUND_FILE};
use crate::model::{Chapter, PolishStatus};
use crate::state;
use std::path::{Path, PathBuf};

pub(super) fn round_path(state_dir: &Path, project_id: &str) -> PathBuf {
    state::project_dir(state_dir, project_id).join(ROUND_FILE)
}

/// 统计仍有草稿但尚未润色成功的段落数。
pub fn pending_segment_count(chapters: &[Chapter]) -> usize {
    chapters
        .iter()
        .flat_map(|chapter| &chapter.segments)
        .filter(|segment| {
            segment.polish_status != Some(PolishStatus::Succeeded)
                && segment
                    .target_before_polish
                    .as_deref()
                    .is_some_and(|draft| !draft.trim().is_empty())
        })
        .count()
}

/// 读取当前轮次的汇总；轮次文件不存在时返回 `None`。
pub fn read_summary(
    state_dir: &Path,
    project_id: &str,
    chapters: &[Chapter],
) -> Result<Option<PolishSummary>, String> {
    let path = round_path(state_dir, project_id);
    let pending_segments = pending_segment_count(chapters);
    if !path.is_file() {
        return Ok(None);
    }
    let round: PolishRound = state::read_json(&path)?;
    let mut summary = round.summary();
    summary.pending_segments = pending_segments;
    Ok(Some(summary))
}

/// 删除轮次文件，使下次润色重新建立快照。
pub fn invalidate_round(state_dir: &Path, project_id: &str) -> Result<(), String> {
    let path = round_path(state_dir, project_id);
    if !path.exists() {
        return Ok(());
    }
    std::fs::remove_file(&path).map_err(|error| {
        format!(
            "failed to invalidate polish round {}: {error}",
            path.display()
        )
    })
}
