//! 术语与重译命令：裁定术语、扫描影响范围、恢复译文与单条重译。

use super::dto::{AffectedContent, ProjectDetail, RetranslationProgressEvent};
use super::project::project_detail;
use super::TaskRegistry;
use crate::config;
use crate::model::{Chapter, SegmentKind};
use crate::polish;
use crate::state;
use crate::terms::{TermPolicy, TermStore};
use crate::workflow;
use std::path::PathBuf;
use tauri::Emitter;

/// 人工裁定术语译名，并让旧润色快照失效。
#[tauri::command]
pub fn ui_resolve_term(project_id: String, source: String, target: String) -> Result<(), String> {
    let loaded = config::load(None)?;
    let project = state::load_project(&loaded.state_dir, &project_id)?;
    let store =
        TermStore::open(state::project_dir(&loaded.state_dir, &project.id).join("terms.db"))?;
    store.resolve(&source, &target)?;
    polish::invalidate_round(&loaded.state_dir, &project.id)?;
    state::append_log(
        &loaded.state_dir,
        &project,
        "term_resolved",
        serde_json::json!({ "source": source, "target": target }),
    )
}

/// 设置术语策略。
#[tauri::command]
pub fn ui_set_term_policy(
    project_id: String,
    source: String,
    policy: TermPolicy,
) -> Result<(), String> {
    let loaded = config::load(None)?;
    let project = state::load_project(&loaded.state_dir, &project_id)?;
    let store =
        TermStore::open(state::project_dir(&loaded.state_dir, &project.id).join("terms.db"))?;
    store.set_policy(&source, policy)?;
    polish::invalidate_round(&loaded.state_dir, &project.id)?;
    state::append_log(
        &loaded.state_dir,
        &project,
        "term_policy_changed",
        serde_json::json!({ "source": source, "policy": policy }),
    )
}

/// 撤销人工裁定。
#[tauri::command]
pub fn ui_undo_term_resolution(project_id: String, source: String) -> Result<(), String> {
    let loaded = config::load(None)?;
    let project = state::load_project(&loaded.state_dir, &project_id)?;
    let store =
        TermStore::open(state::project_dir(&loaded.state_dir, &project.id).join("terms.db"))?;
    store.undo_resolution(&source)?;
    polish::invalidate_round(&loaded.state_dir, &project.id)?;
    state::append_log(
        &loaded.state_dir,
        &project,
        "term_resolution_undone",
        serde_json::json!({ "source": source }),
    )
}

/// 扫描术语在全书中的影响范围。
#[tauri::command]
pub fn ui_scan_term_impact(
    project_id: String,
    source: String,
) -> Result<Vec<AffectedContent>, String> {
    let loaded = config::load(None)?;
    let project = state::load_project(&loaded.state_dir, &project_id)?;
    let store =
        TermStore::open(state::project_dir(&loaded.state_dir, &project.id).join("terms.db"))?;
    if !store.list()?.iter().any(|term| term.source == source) {
        return Err(format!("term not found: {source}"));
    }
    let chapters = state::load_chapters(&loaded.state_dir, &project)?;
    Ok(scan_term_impact(&chapters, &source))
}

/// 匹配术语命中的章节标题与段落；标题段落不重复计入。
pub(super) fn scan_term_impact(chapters: &[Chapter], source: &str) -> Vec<AffectedContent> {
    let mut affected = Vec::new();
    for (chapter_index, chapter) in chapters.iter().enumerate() {
        let title_matches = crate::terms::matches_text(&chapter.title, source);
        if title_matches {
            if let Some(target) = &chapter.target_title {
                affected.push(AffectedContent {
                    id: format!("title:{}", chapter.id),
                    chapter_id: chapter.id.clone(),
                    chapter: chapter_index,
                    kind: "title".to_string(),
                    source: chapter.title.clone(),
                    current_target: target.clone(),
                    previous_target: chapter
                        .meta
                        .get("previous_target_title")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                    retranslation_error: chapter
                        .meta
                        .get("retranslation_error")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                });
            }
        }
        for segment in &chapter.segments {
            if title_matches
                && segment.kind == SegmentKind::Heading
                && segment.source.trim() == chapter.title.trim()
            {
                continue;
            }
            if !crate::terms::matches_text(&segment.source, source) {
                continue;
            }
            let Some(target) = &segment.target else {
                continue;
            };
            affected.push(AffectedContent {
                id: segment.id.clone(),
                chapter_id: chapter.id.clone(),
                chapter: chapter_index,
                kind: format!("{:?}", segment.kind).to_ascii_lowercase(),
                source: segment.source.clone(),
                current_target: target.clone(),
                previous_target: segment
                    .meta
                    .get("previous_target")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
                retranslation_error: segment
                    .meta
                    .get("retranslation_error")
                    .and_then(|value| value.as_str())
                    .map(str::to_string),
            });
        }
    }
    affected
}

/// 把选中的译文恢复为重译前的版本。
#[tauri::command]
pub fn ui_restore_translation(project_id: String, item_id: String) -> Result<(), String> {
    let loaded = config::load(None)?;
    let project = state::load_project(&loaded.state_dir, &project_id)?;
    let _lock = state::acquire_project_lock(&loaded.state_dir, &project)?;
    let mut chapters = state::load_chapters(&loaded.state_dir, &project)?;
    let chapter_index = restore_translation(&mut chapters, &item_id)?;
    state::write_chapter(&loaded.state_dir, &project, &chapters[chapter_index])?;
    polish::invalidate_round(&loaded.state_dir, &project.id)?;
    state::append_log(
        &loaded.state_dir,
        &project,
        "translation_restored",
        serde_json::json!({ "item_id": item_id }),
    )
}

/// 交换 `previous_target` 与当前译文；标题条目会同步标题段落。
pub(super) fn restore_translation(
    chapters: &mut [Chapter],
    item_id: &str,
) -> Result<usize, String> {
    if let Some(chapter_id) = item_id.strip_prefix("title:") {
        let chapter_index = chapters
            .iter()
            .position(|chapter| chapter.id == chapter_id)
            .ok_or_else(|| format!("chapter was removed: {chapter_id}"))?;
        let chapter = &mut chapters[chapter_index];
        let previous = chapter
            .meta
            .get("previous_target_title")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .ok_or_else(|| "没有可恢复的旧译文".to_string())?;
        let current = chapter.target_title.replace(previous.clone());
        if let Some(current) = current {
            chapter.meta["previous_target_title"] = serde_json::Value::String(current);
        }
        for segment in &mut chapter.segments {
            if segment.kind == SegmentKind::Heading && segment.source.trim() == chapter.title.trim()
            {
                segment.target = Some(previous.clone());
                segment.polish_status = segment
                    .target_before_polish
                    .is_some()
                    .then_some(crate::model::PolishStatus::Pending);
            }
        }
        Ok(chapter_index)
    } else {
        let chapter_index = chapters
            .iter()
            .position(|chapter| chapter.segments.iter().any(|segment| segment.id == item_id))
            .ok_or_else(|| format!("content was removed: {item_id}"))?;
        let chapter = &mut chapters[chapter_index];
        let segment = chapter
            .segments
            .iter_mut()
            .find(|segment| segment.id == item_id)
            .unwrap();
        let previous = segment
            .meta
            .get("previous_target")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .ok_or_else(|| "没有可恢复的旧译文".to_string())?;
        let current = segment.target.replace(previous.clone());
        if let Some(current) = current {
            segment.meta["previous_target"] = serde_json::Value::String(current);
        }
        segment.polish_status = segment
            .target_before_polish
            .is_some()
            .then_some(crate::model::PolishStatus::Pending);
        segment
            .meta
            .as_object_mut()
            .map(|meta| meta.remove("retranslation_error"));
        if segment.kind == SegmentKind::Heading && segment.source.trim() == chapter.title.trim() {
            chapter.target_title = Some(previous);
        }
        Ok(chapter_index)
    }
}

/// 重译选中的条目，并在每个条目完成时推送进度事件。
#[tauri::command]
pub async fn ui_retranslate(
    app: tauri::AppHandle,
    registry: tauri::State<'_, TaskRegistry>,
    project_id: String,
    item_ids: Vec<String>,
    mock_client: bool,
) -> Result<ProjectDetail, String> {
    if item_ids.is_empty() {
        return Err("请至少选择一项可能受影响的内容".to_string());
    }
    if registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .contains_key(&project_id)
    {
        return Err("当前翻译任务运行中，请等待任务结束后再重译".to_string());
    }
    let loaded = config::load(None)?;
    let project = state::load_project(&loaded.state_dir, &project_id)?;
    let task_id = project_id.clone();
    let (progress_sender, mut progress_receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut task = tokio::spawn(workflow::retranslate_project(
        None,
        PathBuf::from(project.source_path),
        item_ids,
        mock_client,
        Some(progress_sender),
    ));
    registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .insert(task_id.clone(), task.abort_handle());
    let mut progress_open = true;
    let result = loop {
        tokio::select! {
            biased;
            progress = progress_receiver.recv(), if progress_open => match progress {
                Some(progress) => {
                    if let Ok(detail) = project_detail(&loaded.state_dir, &project_id) {
                        let _ = app.emit("retranslation-progress", RetranslationProgressEvent {
                            project_id: project_id.clone(),
                            completed: progress.completed,
                            total: progress.total,
                            succeeded: progress.succeeded,
                            failed: progress.failed,
                            item_id: progress.item_id,
                            detail,
                        });
                    }
                }
                None => progress_open = false,
            },
            result = &mut task => break result,
        }
    };
    registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .remove(&task_id);
    let project = result.map_err(|error| format!("retranslation task failed: {error}"))??;
    project_detail(&loaded.state_dir, &project.id)
}
