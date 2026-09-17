//! 项目初始化：导入源文件、解析并运行全书分析。

use super::common::build_client;
use crate::analysis;
use crate::config;
use crate::model::ProjectState;
use crate::parser;
use crate::polish;
use crate::state;
use std::path::PathBuf;

/// 初始化项目并完成全书分析。
///
/// 相同源文件已有项目时直接复用；`force_analysis` 为 true 时重新分析，
/// 并让旧的润色轮次失效（风格指南和梗概已变化）。
pub async fn initialize_project(
    config_path: Option<PathBuf>,
    input: PathBuf,
    source_language: Option<String>,
    max_segment_chars: Option<usize>,
    mock_client: bool,
    force_analysis: bool,
) -> Result<(ProjectState, bool), String> {
    if !input.is_file() {
        return Err(format!("input file does not exist: {}", input.display()));
    }
    let loaded = config::load(config_path.as_deref())?;
    let state_dir = loaded.state_dir;
    let config = loaded.value;
    let existing = state::load_for_source(&state_dir, &input).ok();
    let (mut project, mut chapters, created) = if let Some(project) = existing {
        let chapters = state::load_chapters(&state_dir, &project)?;
        (project, chapters, false)
    } else {
        let requested_language = source_language
            .as_deref()
            .unwrap_or(&config.language.source);
        let max_segment_chars = max_segment_chars.unwrap_or(config.segment.max_chars_per_segment);
        let mut document =
            parser::parse_document(&input, Some(requested_language), max_segment_chars)?;
        document.metadata.target_language = config.language.target.clone();
        let initialized = state::initialize(&state_dir, &input, &document, max_segment_chars)?;
        let chapters = state::load_chapters(&state_dir, &initialized.project)?;
        (initialized.project, chapters, initialized.created)
    };
    let _lock = state::acquire_project_lock(&state_dir, &project)?;
    if let Some(source_language) = source_language {
        project.source_language = source_language;
        state::save_project(&state_dir, &project)?;
    }
    let result = async {
        let client = build_client(&config, mock_client, &state_dir, &project)?;
        analysis::prepare(
            client.as_ref(),
            &state_dir,
            &mut project,
            &mut chapters,
            config.analysis.full_book,
            force_analysis,
            config.llm.max_retries,
        )
        .await
    }
    .await;
    if let Err(error) = result {
        state::mark_failed(&state_dir, &mut project, &error).map_err(|log_error| {
            format!("{error}; failed to record initialization error: {log_error}")
        })?;
        return Err(error);
    }
    state::append_log(
        &state_dir,
        &project,
        "analysis_completed",
        serde_json::json!({ "full_book": config.analysis.full_book }),
    )?;
    if force_analysis {
        // 重新分析会改变润色快照里的风格指南和梗概，旧轮次必须作废。
        polish::invalidate_round(&state_dir, &project.id)?;
    }
    Ok((project, created))
}

/// 仅导入并解析源文件，不运行模型分析。
pub fn import_project(
    config_path: Option<PathBuf>,
    input: PathBuf,
) -> Result<(ProjectState, bool), String> {
    if !input.is_file() {
        return Err(format!("input file does not exist: {}", input.display()));
    }
    let loaded = config::load(config_path.as_deref())?;
    if let Ok(project) = state::load_for_source(&loaded.state_dir, &input) {
        return Ok((project, false));
    }

    let config = loaded.value;
    let mut document = parser::parse_document(
        &input,
        Some(&config.language.source),
        config.segment.max_chars_per_segment,
    )?;
    document.metadata.target_language = config.language.target;
    let initialized = state::initialize(
        &loaded.state_dir,
        &input,
        &document,
        config.segment.max_chars_per_segment,
    )?;
    Ok((initialized.project, initialized.created))
}
