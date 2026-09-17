//! 翻译与润色命令：以后台任务运行流程，并把进度事件推送给前端。

use super::dto::{ProgressSnapshot, ProjectDetail};
use super::project::project_detail;
use super::TaskRegistry;
use crate::config;
use crate::state;
use crate::workflow;
use std::path::PathBuf;
use std::time::Duration;
use tauri::Emitter;

/// 翻译全书或指定章节，运行期间每 500ms 推送一次进度事件。
#[tauri::command]
pub async fn ui_transit(
    app: tauri::AppHandle,
    registry: tauri::State<'_, TaskRegistry>,
    project_id: String,
    chapter: Option<usize>,
    mock_client: bool,
) -> Result<ProjectDetail, String> {
    if registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .contains_key(&project_id)
    {
        return Err("当前项目有任务运行中，请等待任务结束后再翻译".to_string());
    }
    let loaded = config::load(None)?;
    let project = state::load_project(&loaded.state_dir, &project_id)?;
    let task_id = project_id.clone();
    let mut task = tokio::spawn(workflow::transit_project(
        None,
        PathBuf::from(&project.source_path),
        chapter,
        mock_client,
    ));
    registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .insert(task_id.clone(), task.abort_handle());
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_snapshot = None;
    let result = loop {
        tokio::select! {
            result = &mut task => break result,
            _ = interval.tick() => {
                if let Ok(detail) = project_detail(&loaded.state_dir, &project_id) {
                    let snapshot = ProgressSnapshot::from(&detail);
                    if last_snapshot.as_ref() != Some(&snapshot) {
                        let _ = app.emit("translation-progress", &detail);
                        last_snapshot = Some(snapshot);
                    }
                }
            }
        }
    };
    registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .remove(&task_id);
    let project = match result {
        Ok(Ok(project)) => project,
        Ok(Err(error)) => {
            let _ = state::append_log(
                &loaded.state_dir,
                &project,
                "ui_translation_failed",
                serde_json::json!({ "error": error }),
            );
            return Err(error);
        }
        Err(error) if error.is_cancelled() => {
            let _ = state::append_log(
                &loaded.state_dir,
                &project,
                "ui_translation_cancelled",
                serde_json::json!({}),
            );
            return Err("翻译任务已取消，可重新运行以断点续译".to_string());
        }
        Err(error) => {
            let detail = format!("translation task failed: {error}");
            let _ = state::append_log(
                &loaded.state_dir,
                &project,
                "ui_translation_task_failed",
                serde_json::json!({ "error": detail }),
            );
            return Err(detail);
        }
    };
    let detail = project_detail(&loaded.state_dir, &project.id)?;
    let _ = app.emit("translation-progress", &detail);
    Ok(detail)
}

/// 运行润色，运行期间每 500ms 推送一次进度事件。
#[tauri::command]
pub async fn ui_polish(
    app: tauri::AppHandle,
    registry: tauri::State<'_, TaskRegistry>,
    project_id: String,
    retry_failed: bool,
    mock_client: bool,
) -> Result<ProjectDetail, String> {
    if registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .contains_key(&project_id)
    {
        return Err("当前项目有任务运行中，请等待任务结束后再润色".to_string());
    }
    let loaded = config::load(None)?;
    let project = state::load_project(&loaded.state_dir, &project_id)?;
    let task_id = project_id.clone();
    let mut task = tokio::spawn(workflow::polish_project(
        None,
        PathBuf::from(&project.source_path),
        retry_failed,
        mock_client,
    ));
    registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .insert(task_id.clone(), task.abort_handle());
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_snapshot = None;
    let result = loop {
        tokio::select! {
            result = &mut task => break result,
            _ = interval.tick() => {
                if let Ok(detail) = project_detail(&loaded.state_dir, &project_id) {
                    let snapshot = ProgressSnapshot::from(&detail);
                    if last_snapshot.as_ref() != Some(&snapshot) {
                        let _ = app.emit("polish-progress", &detail);
                        last_snapshot = Some(snapshot);
                    }
                }
            }
        }
    };
    registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .remove(&task_id);
    let project = match result {
        Ok(Ok(project)) => project,
        Ok(Err(error)) => {
            let _ = state::append_log(
                &loaded.state_dir,
                &project,
                "polish_failed",
                serde_json::json!({ "error": error }),
            );
            return Err(error);
        }
        Err(error) if error.is_cancelled() => {
            let _ = state::append_log(
                &loaded.state_dir,
                &project,
                "polish_cancelled",
                serde_json::json!({}),
            );
            return Err("润色任务已取消，已保存的结果会保留".to_string());
        }
        Err(error) => {
            let detail = format!("polish task failed: {error}");
            let _ = state::append_log(
                &loaded.state_dir,
                &project,
                "polish_failed",
                serde_json::json!({ "error": detail }),
            );
            return Err(detail);
        }
    };
    let detail = project_detail(&loaded.state_dir, &project.id)?;
    let _ = app.emit("polish-progress", &detail);
    Ok(detail)
}

/// 取消指定任务。
#[tauri::command]
pub fn ui_cancel_task(
    registry: tauri::State<'_, TaskRegistry>,
    task_id: String,
) -> Result<bool, String> {
    let task = registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .remove(&task_id);
    if let Some(task) = task {
        task.abort();
        Ok(true)
    } else {
        Ok(false)
    }
}
