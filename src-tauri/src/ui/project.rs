//! 项目相关命令：启动数据、项目详情、导入、初始化、重新分析与删除。

use super::dto::{Bootstrap, CredentialStatus, LogEntry, ProjectDetail, ProjectSummary};
use super::TaskRegistry;
use crate::config::{self, AppConfig};
use crate::model::ProjectState;
use crate::parser;
use crate::polish;
use crate::state;
use crate::terms::TermStore;
use base64::Engine;
use std::fs;
use std::path::{Path, PathBuf};

/// 返回应用启动所需的配置与项目列表。
#[tauri::command]
pub fn ui_bootstrap() -> Result<Bootstrap, String> {
    let loaded = config::load(None)?;
    let mut projects = list_projects(&loaded.state_dir)?;
    projects.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    let projects = projects
        .into_iter()
        .map(|project| project_summary(&loaded.state_dir, project))
        .collect();
    Ok(Bootstrap {
        credential: credential_status(&loaded.value)?,
        config: loaded.value,
        config_path: loaded.path.map(|path| path.to_string_lossy().into_owned()),
        state_dir: loaded.state_dir.to_string_lossy().into_owned(),
        projects,
    })
}

fn project_summary(state_dir: &Path, project: ProjectState) -> ProjectSummary {
    let cover_data_url = parser::extract_epub_cover(Path::new(&project.source_path))
        .ok()
        .flatten()
        .map(|cover| {
            let encoded = base64::engine::general_purpose::STANDARD.encode(cover.bytes);
            format!("data:{};base64,{encoded}", cover.media_type)
        });
    ProjectSummary {
        task_initialized: initialization_completed(state_dir, &project),
        project,
        cover_data_url,
    }
}

/// 加载单个项目的详情。
#[tauri::command]
pub fn ui_project(project_id: String) -> Result<ProjectDetail, String> {
    let loaded = config::load(None)?;
    project_detail(&loaded.state_dir, &project_id)
}

/// 删除项目数据；运行中的项目需要先取消任务。
#[tauri::command]
pub fn ui_delete_project(
    registry: tauri::State<'_, TaskRegistry>,
    project_id: String,
) -> Result<Bootstrap, String> {
    let has_running_task = registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .contains_key(&project_id);
    if has_running_task {
        return Err("项目正在运行任务，请先取消任务再删除".to_string());
    }

    let loaded = config::load(None)?;
    delete_project_at(&loaded.state_dir, &project_id)?;
    ui_bootstrap()
}

/// 仅导入并解析源文件，不运行分析。
#[tauri::command]
pub async fn ui_import(
    registry: tauri::State<'_, TaskRegistry>,
    input: String,
) -> Result<ProjectDetail, String> {
    let task_id = "import".to_string();
    let task =
        tokio::spawn(async move { crate::workflow::import_project(None, PathBuf::from(input)) });
    registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .insert(task_id.clone(), task.abort_handle());
    let result = task.await;
    registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .remove(&task_id);
    let (project, _) = result.map_err(|error| {
        if error.is_cancelled() {
            "书籍导入已取消".to_string()
        } else {
            format!("import task failed: {error}")
        }
    })??;
    let loaded = config::load(None)?;
    project_detail(&loaded.state_dir, &project.id)
}

/// 运行项目初始化（分析全书）。
#[tauri::command]
pub async fn ui_initialize(
    registry: tauri::State<'_, TaskRegistry>,
    project_id: String,
    mock_client: bool,
) -> Result<ProjectDetail, String> {
    let loaded = config::load(None)?;
    let project = state::load_project(&loaded.state_dir, &project_id)?;
    let source_language = loaded.value.language.source;
    let task_id = "initialize".to_string();
    let task = tokio::spawn(crate::workflow::initialize_project(
        None,
        PathBuf::from(project.source_path),
        Some(source_language),
        None,
        mock_client,
        false,
    ));
    registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .insert(task_id.clone(), task.abort_handle());
    let result = task.await;
    registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .remove(&task_id);
    let (project, _) = result.map_err(|error| {
        if error.is_cancelled() {
            "项目初始化已取消".to_string()
        } else {
            format!("initialization task failed: {error}")
        }
    })??;
    let loaded = config::load(None)?;
    project_detail(&loaded.state_dir, &project.id)
}

/// 强制重新分析，并让旧的润色快照失效。
#[tauri::command]
pub async fn ui_reanalyze(
    registry: tauri::State<'_, TaskRegistry>,
    project_id: String,
    mock_client: bool,
) -> Result<ProjectDetail, String> {
    let loaded = config::load(None)?;
    let project = state::load_project(&loaded.state_dir, &project_id)?;
    let source_language = loaded.value.language.source;
    let task_id = "initialize".to_string();
    let task = tokio::spawn(crate::workflow::initialize_project(
        None,
        PathBuf::from(project.source_path),
        Some(source_language),
        None,
        mock_client,
        true,
    ));
    registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .insert(task_id.clone(), task.abort_handle());
    let result = task.await;
    registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .remove(&task_id);
    let (project, _) = result.map_err(|error| {
        if error.is_cancelled() {
            "重新分析已取消".to_string()
        } else {
            format!("analysis task failed: {error}")
        }
    })??;
    let loaded = config::load(None)?;
    project_detail(&loaded.state_dir, &project.id)
}

/// 组装项目详情：章节、术语、冲突、报告、日志与润色汇总。
pub(super) fn project_detail(state_dir: &Path, project_id: &str) -> Result<ProjectDetail, String> {
    let project = state::load_project(state_dir, project_id)?;
    let chapters = state::load_chapters(state_dir, &project)?;
    let directory = state::project_dir(state_dir, &project.id);
    let terms_path = directory.join("terms.db");
    let (terms, conflicts, term_conflicts) = if terms_path.exists() {
        let store = TermStore::open(terms_path)?;
        (
            store.list()?,
            store.conflicts()?,
            store.conflict_details(true)?,
        )
    } else {
        (Vec::new(), Vec::new(), Vec::new())
    };
    let report_path = directory.join("report.json");
    let report = if report_path.exists() {
        Some(state::read_json(&report_path)?)
    } else {
        None
    };
    let logs = read_logs(&directory.join("logs.txt"))?;
    let task_initialized = initialization_completed(state_dir, &project);
    let polish = polish::read_summary(state_dir, &project.id, &chapters).unwrap_or(None);
    Ok(ProjectDetail {
        project,
        task_initialized,
        chapters,
        logs,
        terms,
        conflicts,
        pending_conflicts: term_conflicts
            .iter()
            .map(|conflict| conflict.unresolved_events)
            .sum(),
        term_conflicts,
        report,
        polish,
    })
}

/// 读取状态目录下的全部项目。
fn list_projects(state_dir: &Path) -> Result<Vec<ProjectState>, String> {
    if !state_dir.exists() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(state_dir)
        .map_err(|error| format!("failed to read state directory: {error}"))?;
    Ok(entries
        .filter_map(Result::ok)
        .filter_map(|entry| state::read_json(&entry.path().join("project.json")).ok())
        .collect())
}

/// 删除项目目录：先移动到 `.trash` 再删除，避免与运行中任务互相影响。
pub(super) fn delete_project_at(state_dir: &Path, project_id: &str) -> Result<(), String> {
    let project = state::load_project(state_dir, project_id)?;
    let normalized_id = project_id.to_ascii_lowercase();
    if project.id != normalized_id {
        return Err("project metadata does not match its directory".to_string());
    }

    let lock = state::acquire_project_lock(state_dir, &project)?;
    let project_dir = state::project_dir(state_dir, &normalized_id);
    let trash_dir = state_dir.join(".trash");
    fs::create_dir_all(&trash_dir)
        .map_err(|error| format!("failed to create project trash directory: {error}"))?;
    let staged_dir = trash_dir.join(format!(
        "{}-{}-{}",
        normalized_id,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    ));
    fs::rename(&project_dir, &staged_dir)
        .map_err(|error| format!("failed to stage project for deletion: {error}"))?;
    drop(lock);
    fs::remove_dir_all(&staged_dir)
        .map_err(|error| format!("failed to delete project data: {error}"))
}

/// 汇总凭据来源：本地免密钥、桌面端存储或环境变量。
pub(super) fn credential_status(config: &AppConfig) -> Result<CredentialStatus, String> {
    if config.llm.allows_empty_key() {
        return Ok(CredentialStatus {
            configured: true,
            source: Some("none"),
        });
    }
    let stored = crate::credentials::load_api_key(&config.llm.provider)?;
    if stored.is_some() {
        return Ok(CredentialStatus {
            configured: true,
            source: Some("desktop"),
        });
    }
    if let Ok(value) = std::env::var(&config.llm.api_key_env) {
        if !value.trim().is_empty() {
            return Ok(CredentialStatus {
                configured: true,
                source: Some("environment"),
            });
        }
    }
    Ok(CredentialStatus {
        configured: false,
        source: None,
    })
}

/// 日志中是否出现过 `analysis_completed`，用于判断项目是否已完成分析。
fn initialization_completed(state_dir: &Path, project: &ProjectState) -> bool {
    let path = state::project_dir(state_dir, &project.id).join("logs.txt");
    fs::read_to_string(path).is_ok_and(|text| {
        text.lines()
            .any(|line| line.split('\t').nth(1) == Some("analysis_completed"))
    })
}

/// 读取最近 100 条日志，最新的排在最前。
pub(super) fn read_logs(path: &Path) -> Result<Vec<LogEntry>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let mut logs = text
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            Some(LogEntry {
                timestamp: fields.next()?.to_string(),
                event: fields.next()?.to_string(),
                details: serde_json::from_str(fields.next()?).unwrap_or(serde_json::Value::Null),
            })
        })
        .collect::<Vec<_>>();
    logs.reverse();
    logs.truncate(100);
    Ok(logs)
}
