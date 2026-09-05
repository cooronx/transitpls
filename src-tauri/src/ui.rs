use crate::config::{self, AppConfig};
use crate::export::{self, ExportFormat};
use crate::llm::{RigClient, TranslationClient};
use crate::model::{Chapter, ProjectState};
use crate::state;
use crate::terms::{Term, TermCandidate, TermStore};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tokio::task::AbortHandle;

#[derive(Default)]
pub struct TaskRegistry {
    tasks: Mutex<HashMap<String, AbortHandle>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    config: AppConfig,
    credential: CredentialStatus,
    config_path: Option<String>,
    state_dir: String,
    projects: Vec<ProjectState>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatus {
    configured: bool,
    source: Option<&'static str>,
    last_four: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDetail {
    project: ProjectState,
    chapters: Vec<Chapter>,
    logs: Vec<LogEntry>,
    terms: Vec<Term>,
    conflicts: Vec<TermCandidate>,
    report: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct LogEntry {
    timestamp: String,
    event: String,
    details: serde_json::Value,
}

#[tauri::command]
pub fn ui_bootstrap() -> Result<Bootstrap, String> {
    let loaded = config::load(None)?;
    let mut projects = list_projects(&loaded.state_dir)?;
    projects.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    Ok(Bootstrap {
        credential: credential_status(&loaded.value)?,
        config: loaded.value,
        config_path: loaded.path.map(|path| path.to_string_lossy().into_owned()),
        state_dir: loaded.state_dir.to_string_lossy().into_owned(),
        projects,
    })
}

#[tauri::command]
pub async fn ui_verify_and_save_model(
    value: AppConfig,
    api_key: Option<String>,
) -> Result<Bootstrap, String> {
    let supplied = api_key.filter(|key| !key.trim().is_empty());
    let key = match supplied.as_deref() {
        Some(key) => key.trim().to_string(),
        None => value.llm.api_key()?,
    };
    let client = RigClient::from_config_with_api_key(&value.llm, key)?;
    client
        .complete(
            "You are checking whether an LLM connection is available. Reply with OK only.",
            "OK",
        )
        .await
        .map_err(|error| format!("模型连接验证失败：{error}"))?;
    if let Some(key) = supplied {
        crate::credentials::save_api_key(&value.llm.provider, &key)?;
    }
    config::save_default(&value)?;
    ui_bootstrap()
}

#[tauri::command]
pub fn ui_project(project_id: String) -> Result<ProjectDetail, String> {
    let loaded = config::load(None)?;
    project_detail(&loaded.state_dir, &project_id)
}

#[tauri::command]
pub async fn ui_initialize(
    registry: tauri::State<'_, TaskRegistry>,
    input: String,
    mock_client: bool,
) -> Result<ProjectDetail, String> {
    let task_id = "initialize".to_string();
    let task = tokio::spawn(crate::cli::initialize_project(
        None,
        PathBuf::from(input),
        None,
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

#[tauri::command]
pub async fn ui_transit(
    registry: tauri::State<'_, TaskRegistry>,
    project_id: String,
    chapter: Option<usize>,
    mock_client: bool,
) -> Result<ProjectDetail, String> {
    let loaded = config::load(None)?;
    let project = state::load_project(&loaded.state_dir, &project_id)?;
    let task_id = project_id.clone();
    let task = tokio::spawn(crate::cli::transit_project(
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
    let result = task.await;
    registry
        .tasks
        .lock()
        .map_err(|_| "task registry lock is poisoned".to_string())?
        .remove(&task_id);
    let project = result.map_err(|error| {
        if error.is_cancelled() {
            "翻译任务已取消，可重新运行以断点续译".to_string()
        } else {
            format!("translation task failed: {error}")
        }
    })??;
    project_detail(&loaded.state_dir, &project.id)
}

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

#[tauri::command]
pub fn ui_resolve_term(project_id: String, source: String, target: String) -> Result<(), String> {
    let loaded = config::load(None)?;
    let project = state::load_project(&loaded.state_dir, &project_id)?;
    let _lock = state::acquire_project_lock(&loaded.state_dir, &project)?;
    let store =
        TermStore::open(state::project_dir(&loaded.state_dir, &project.id).join("terms.db"))?;
    store.resolve(&source, &target)?;
    state::append_log(
        &loaded.state_dir,
        &project,
        "term_resolved",
        serde_json::json!({ "source": source, "target": target }),
    )
}

#[tauri::command]
pub fn ui_export(project_id: String, format: String) -> Result<String, String> {
    let loaded = config::load(None)?;
    let project = state::load_project(&loaded.state_dir, &project_id)?;
    let input = PathBuf::from(&project.source_path);
    let format = match format.as_str() {
        "txt" => ExportFormat::Txt,
        "epub" => ExportFormat::Epub,
        _ => return Err("export format must be txt or epub".to_string()),
    };
    let output = export::default_output_path(&input, format);
    let snapshot = state::load_export_snapshot(&loaded.state_dir, &input)?;
    let bytes = match format {
        ExportFormat::Txt => export::render_txt(&snapshot)?.into_bytes(),
        ExportFormat::Epub => export::render_epub(&snapshot)?,
    };
    export::write_atomic(&output, &bytes)?;
    state::append_log(
        &loaded.state_dir,
        &project,
        "exported",
        serde_json::json!({ "format": format.extension(), "output": output }),
    )?;
    Ok(output.to_string_lossy().into_owned())
}

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

fn credential_status(config: &AppConfig) -> Result<CredentialStatus, String> {
    if let Ok(value) = std::env::var(&config.llm.api_key_env) {
        if !value.trim().is_empty() {
            return Ok(CredentialStatus {
                configured: true,
                source: Some("environment"),
                last_four: Some(last_four(&value)),
            });
        }
    }
    let stored = crate::credentials::load_api_key(&config.llm.provider)?;
    Ok(CredentialStatus {
        configured: stored.is_some(),
        source: stored.as_ref().map(|_| "desktop"),
        last_four: stored.as_deref().map(last_four),
    })
}

fn last_four(value: &str) -> String {
    value
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

fn project_detail(state_dir: &Path, project_id: &str) -> Result<ProjectDetail, String> {
    let project = state::load_project(state_dir, project_id)?;
    let chapters = state::load_chapters(state_dir, &project)?;
    let directory = state::project_dir(state_dir, &project.id);
    let terms_path = directory.join("terms.db");
    let (terms, conflicts) = if terms_path.exists() {
        let store = TermStore::open(terms_path)?;
        (store.list()?, store.conflicts()?)
    } else {
        (Vec::new(), Vec::new())
    };
    let report_path = directory.join("report.json");
    let report = if report_path.exists() {
        Some(state::read_json(&report_path)?)
    } else {
        None
    };
    Ok(ProjectDetail {
        project,
        chapters,
        logs: read_logs(&directory.join("logs.txt"))?,
        terms,
        conflicts,
        report,
    })
}

fn read_logs(path: &Path) -> Result<Vec<LogEntry>, String> {
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

#[cfg(test)]
mod tests {
    use super::{project_detail, read_logs};
    use crate::model::{Chapter, Document, DocumentMetadata, ItemStatus, Segment, SegmentKind};
    use crate::state;
    use serde_json::Value;
    use std::fs;

    #[test]
    fn reads_newest_logs_first() {
        let path = std::env::temp_dir().join(format!(
            "transitpls-ui-log-{}-{}.txt",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::write(
            &path,
            "2026-01-01T00:00:00Z\tstarted\t{}\n2026-01-01T00:00:01Z\tdone\t{\"count\":1}\n",
        )
        .expect("fixture should be written");
        let logs = read_logs(&path).expect("logs should parse");
        assert_eq!(logs[0].event, "done");
        assert_eq!(logs[1].event, "started");
        fs::remove_file(path).expect("fixture should be removed");
    }

    #[test]
    fn project_detail_reads_the_same_state_as_the_cli() {
        let root = std::env::temp_dir().join(format!(
            "transitpls-ui-project-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::create_dir_all(&root).expect("fixture directory should be created");
        let input = root.join("book.txt");
        fs::write(&input, "Hello world").expect("source should be written");
        let document = Document {
            metadata: DocumentMetadata {
                title: "Book".to_string(),
                source_language: "en".to_string(),
                target_language: "zh-CN".to_string(),
                source_format: "txt".to_string(),
            },
            chapters: vec![Chapter {
                id: "chapter-0000-book".to_string(),
                title: "Book".to_string(),
                target_title: None,
                status: ItemStatus::Pending,
                meta: Value::Null,
                segments: vec![Segment {
                    id: "segment-0000".to_string(),
                    ordinal: 0,
                    source: "Hello world".to_string(),
                    target: None,
                    target_before_polish: None,
                    kind: SegmentKind::Paragraph,
                    status: ItemStatus::Pending,
                    source_hash: "fixture".to_string(),
                    meta: Value::Null,
                }],
            }],
        };
        let initialized =
            state::initialize(&root, &input, &document, 1_200).expect("project should initialize");

        let detail = project_detail(&root, &initialized.project.id)
            .expect("desktop bridge should load project state");

        assert_eq!(detail.project.title, "Book");
        assert_eq!(detail.chapters[0].segments[0].source, "Hello world");
        assert_eq!(detail.logs[0].event, "initialized");
        fs::remove_dir_all(root).expect("fixture should be removed");
    }
}
