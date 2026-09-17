//! 章节文件的读写与旧项目润色状态兼容。

use super::io::{read_json, write_json_atomic};
use super::project_dir;
use crate::model::{Chapter, ItemStatus, PolishStatus, ProjectState};
use std::fs;
use std::path::Path;

/// 读取项目全部章节，按章节 ID 中的序号排序。
pub fn load_chapters(state_dir: &Path, project: &ProjectState) -> Result<Vec<Chapter>, String> {
    let dir = project_dir(state_dir, &project.id).join("chapters");
    let mut chapters = Vec::new();
    let entries =
        fs::read_dir(&dir).map_err(|error| format!("failed to read chapters: {error}"))?;
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        chapters.push(read_json(&path)?);
    }
    chapters.sort_by_key(|chapter: &Chapter| {
        chapter
            .id
            .strip_prefix("chapter-")
            .and_then(|id| id.split('-').next())
            .and_then(|ordinal| ordinal.parse::<usize>().ok())
            .unwrap_or(usize::MAX)
    });
    normalize_polish_state(&mut chapters);
    Ok(chapters)
}

/// 写回单个章节文件。
pub fn write_chapter(
    state_dir: &Path,
    project: &ProjectState,
    chapter: &Chapter,
) -> Result<(), String> {
    write_chapter_at(&project_dir(state_dir, &project.id), chapter)
}

/// 按项目目录写入章节，初始化期间直接写临时目录时使用。
pub(super) fn write_chapter_at(project_dir: &Path, chapter: &Chapter) -> Result<(), String> {
    write_json_atomic(
        &project_dir
            .join("chapters")
            .join(format!("{}.json", chapter.id)),
        chapter,
    )
}

/// 兼容旧项目数据。
///
/// 早期版本把润色草稿只写在 `target_before_polish`，`target` 保持为空。
/// 这里把草稿恢复为可读译文，并推断当时的润色结果，避免旧项目被要求重新翻译或润色。
pub fn normalize_polish_state(chapters: &mut [Chapter]) {
    for chapter in chapters.iter_mut() {
        for segment in chapter.segments.iter_mut() {
            let Some(draft) = segment
                .target_before_polish
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
            else {
                continue;
            };
            if segment.target.is_none() {
                segment.target = Some(draft);
                segment.status = ItemStatus::Translated;
                if segment.polish_status.is_none() {
                    segment.polish_status = Some(PolishStatus::Pending);
                }
            } else if segment.polish_status.is_none() {
                segment.polish_status = Some(PolishStatus::Succeeded);
            }
        }
    }
}
