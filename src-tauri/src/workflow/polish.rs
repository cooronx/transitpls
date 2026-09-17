//! 润色工作流入口：加载项目并运行一轮润色。

use super::common::{build_client, load_translation_analysis, save_project_progress, term_store};
use crate::config;
use crate::llm::TranslationClient;
use crate::model::ProjectState;
use crate::polish::{self, PolishSummary};
use crate::state;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 界面入口：加载配置后运行润色。
pub async fn polish_project(
    config_path: Option<PathBuf>,
    input: PathBuf,
    retry_failed: bool,
    mock_client: bool,
) -> Result<ProjectState, String> {
    let loaded = config::load(config_path.as_deref())?;
    run_polish(
        input,
        retry_failed,
        mock_client,
        &loaded.state_dir,
        &loaded.value,
    )
    .await
    .map(|(project, _)| project)
}

/// 运行一轮润色，返回更新后的项目与轮次汇总。
///
/// 失败时记录 `polish_failed` 日志；成功只保存进度，汇总日志由调用方决定是否记录。
pub(crate) async fn run_polish(
    input: PathBuf,
    retry_failed: bool,
    mock: bool,
    state_dir: &Path,
    config: &crate::config::AppConfig,
) -> Result<(ProjectState, PolishSummary), String> {
    let mut project = state::load_for_source(state_dir, &input)?;
    let _lock = state::acquire_project_lock(state_dir, &project)?;
    let mut chapters = state::load_chapters(state_dir, &project)?;
    let analysis = load_translation_analysis(state_dir, &project, &chapters, config)?;
    let client: Arc<dyn TranslationClient> =
        Arc::from(build_client(config, mock, state_dir, &project)?);
    let store = term_store(state_dir, &project)?;
    let terms = store.list()?;
    if retry_failed {
        state::append_log(
            state_dir,
            &project,
            "polish_retry_requested",
            serde_json::json!({}),
        )?;
    }
    let result = polish::run_round(
        Arc::clone(&client),
        state_dir,
        &project,
        &mut chapters,
        &analysis,
        &terms,
        config,
        retry_failed,
    )
    .await;
    match result {
        Ok(summary) => {
            save_project_progress(state_dir, &mut project, &chapters)?;
            Ok((project, summary))
        }
        Err(error) => {
            state::append_log(
                state_dir,
                &project,
                "polish_failed",
                serde_json::json!({ "error": error }),
            )?;
            Err(error)
        }
    }
}
