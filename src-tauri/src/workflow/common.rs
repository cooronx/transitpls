//! workflow 子模块共用的辅助函数。

use crate::analysis;
use crate::config::AppConfig;
use crate::llm::{MockClient, RecordingClient, RigClient, TranslationClient};
use crate::model::{Chapter, ItemStatus, ProjectState, ProjectStatus};
use crate::state;
use crate::terms::TermStore;
use crate::usage::UsageRecorder;
use std::path::Path;

/// 按配置创建翻译客户端，并包装用量记录。
pub(super) fn build_client(
    config: &AppConfig,
    mock: bool,
    state_dir: &Path,
    project: &ProjectState,
) -> Result<Box<dyn TranslationClient>, String> {
    let recorder = UsageRecorder::new(
        &state::project_dir(state_dir, &project.id),
        &config.llm.model,
    );
    let inner: Box<dyn TranslationClient> = if mock {
        Box::new(MockClient)
    } else {
        let client = RigClient::from_config(&config.llm).map_err(|error| {
            let _ = recorder.record_event(
                "configuration_failed",
                serde_json::json!({"stage":"configuration","error":error}),
            );
            error
        })?;
        Box::new(client.with_recorder(recorder.clone()))
    };
    Ok(Box::new(RecordingClient::new(inner, recorder)))
}

/// 读取初始化阶段生成的分析结果，并按配置校验完整分析所需的字段。
pub(super) fn load_translation_analysis(
    state_dir: &Path,
    project: &ProjectState,
    chapters: &[Chapter],
    config: &AppConfig,
) -> Result<analysis::BookAnalysis, String> {
    let path = state::project_dir(state_dir, &project.id).join("analysis.json");
    if !path.is_file() {
        return Err(format!(
            "translation analysis is missing at {}; run init first",
            path.display()
        ));
    }
    let value: analysis::BookAnalysis = state::read_json(&path)?;
    if config.analysis.full_book {
        if value
            .book_synopsis
            .as_deref()
            .is_none_or(|synopsis| synopsis.trim().is_empty())
        {
            return Err("book synopsis is missing; run init first".to_string());
        }
        if let Some(chapter) = chapters.iter().find(|chapter| {
            chapter
                .meta
                .get("source_digest")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|digest| digest.trim().is_empty())
        }) {
            return Err(format!(
                "source digest is missing for chapter {}; run init first",
                chapter.id
            ));
        }
    }
    Ok(value)
}

/// 打开项目的术语库。
pub(super) fn term_store(state_dir: &Path, project: &ProjectState) -> Result<TermStore, String> {
    TermStore::open(state::project_dir(state_dir, &project.id).join("terms.db"))
}

/// 章节正文是否全部翻译完成（不含章节标题）。
pub(super) fn chapter_body_complete(chapter: &Chapter) -> bool {
    chapter
        .segments
        .iter()
        .all(|segment| segment.status == ItemStatus::Translated && segment.target.is_some())
}

/// 更新项目完成度与状态并写回 `project.json`。
///
/// 只有全部章节正文完成且所有译文标题就绪时才标记为 `Translated`。
pub(super) fn save_project_progress(
    state_dir: &Path,
    project: &mut ProjectState,
    chapters: &[Chapter],
) -> Result<(), String> {
    project.chapters_completed = chapters
        .iter()
        .filter(|chapter| chapter_body_complete(chapter))
        .count();
    project.updated_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    project.status = if project.chapters_completed == project.chapters_total
        && chapters
            .iter()
            .all(|chapter| chapter.target_title.is_some())
    {
        ProjectStatus::Translated
    } else {
        ProjectStatus::Translating
    };
    state::save_project(state_dir, project)
}
