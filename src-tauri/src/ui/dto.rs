//! 前后端交互的数据结构，字段名按 camelCase 序列化给前端。

use crate::config::AppConfig;
use crate::model::{Chapter, ProjectState};
use crate::polish::PolishSummary;
use crate::terms::{Term, TermCandidate, TermConflict};
use serde::Serialize;

/// 应用启动数据：配置、凭据状态和项目列表。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    pub(crate) config: AppConfig,
    pub(crate) credential: CredentialStatus,
    pub(crate) config_path: Option<String>,
    pub(crate) state_dir: String,
    pub(crate) projects: Vec<ProjectSummary>,
}

/// 项目列表项：项目状态 + 封面 + 是否已完成分析。
#[derive(Debug, Serialize)]
pub struct ProjectSummary {
    #[serde(flatten)]
    pub(crate) project: ProjectState,
    pub(crate) cover_data_url: Option<String>,
    pub(crate) task_initialized: bool,
}

/// API Key 配置状态。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatus {
    pub(crate) configured: bool,
    pub(crate) source: Option<&'static str>,
}

/// 项目详情：工作台需要的全部数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDetail {
    pub(crate) project: ProjectState,
    pub(crate) task_initialized: bool,
    pub(crate) chapters: Vec<Chapter>,
    pub(crate) logs: Vec<LogEntry>,
    pub(crate) terms: Vec<Term>,
    pub(crate) conflicts: Vec<TermCandidate>,
    pub(crate) term_conflicts: Vec<TermConflict>,
    pub(crate) pending_conflicts: usize,
    pub(crate) report: Option<serde_json::Value>,
    pub(crate) polish: Option<PolishSummary>,
}

/// 术语影响扫描结果中的一条内容。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AffectedContent {
    pub(crate) id: String,
    pub(crate) chapter_id: String,
    pub(crate) chapter: usize,
    pub(crate) kind: String,
    pub(crate) source: String,
    pub(crate) current_target: String,
    pub(crate) previous_target: Option<String>,
    pub(crate) retranslation_error: Option<String>,
}

/// 重译进度事件。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RetranslationProgressEvent {
    pub(super) project_id: String,
    pub(super) completed: usize,
    pub(super) total: usize,
    pub(super) succeeded: usize,
    pub(super) failed: usize,
    pub(super) item_id: String,
    pub(super) detail: ProjectDetail,
}

/// 一条项目日志。
#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub(crate) timestamp: String,
    pub(crate) event: String,
    pub(crate) details: serde_json::Value,
}

/// 进度快照，用于判断是否需要向前端推送进度事件。
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ProgressSnapshot {
    updated_at: String,
    translated_segments: usize,
    log_entries: usize,
    polish_succeeded: usize,
    polish_pending: usize,
}

impl From<&ProjectDetail> for ProgressSnapshot {
    fn from(detail: &ProjectDetail) -> Self {
        Self {
            updated_at: detail.project.updated_at.clone(),
            translated_segments: detail
                .chapters
                .iter()
                .flat_map(|chapter| &chapter.segments)
                .filter(|segment| segment.target.is_some())
                .count(),
            log_entries: detail.logs.len(),
            polish_succeeded: detail.polish.as_ref().map_or(0, |polish| polish.succeeded),
            polish_pending: detail.polish.as_ref().map_or(0, |polish| polish.pending),
        }
    }
}
