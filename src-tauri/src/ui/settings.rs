//! 设置相关命令：模型验证与配置保存。

use super::dto::Bootstrap;
use super::project::ui_bootstrap;
use crate::config::{self, AppConfig};
use crate::llm::RigClient;
use crate::llm::TranslationClient;

/// 验证模型连通性，并在成功后保存配置与 API Key。
#[tauri::command]
pub async fn ui_verify_and_save_model(
    mut value: AppConfig,
    api_key: Option<String>,
) -> Result<Bootstrap, String> {
    value.llm = value.llm.normalized()?;
    let supplied = api_key.filter(|key| !key.trim().is_empty());
    if value.llm.allows_empty_key() && supplied.is_some() {
        return Err("configuration_failed stage=configuration: set an API Key environment variable name to enable authentication for a local service".to_string());
    }
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
        .map_err(|error| format!("模型配置验证失败：{error}"))?;
    if let Some(key) = supplied {
        crate::credentials::save_api_key(&value.llm.provider, &key)?;
    }
    config::save_default(&value)?;
    ui_bootstrap()
}

/// 保存界面与并发设置。
#[tauri::command]
pub fn ui_save_general(
    visible_segments: usize,
    retranslation_concurrency: usize,
    polish_concurrency: usize,
) -> Result<Bootstrap, String> {
    let mut loaded = config::load(None)?;
    loaded.value.general.visible_segments = visible_segments;
    loaded.value.general.retranslation_concurrency = retranslation_concurrency;
    loaded.value.general.polish_concurrency = polish_concurrency;
    config::save_default(&loaded.value)?;
    ui_bootstrap()
}

/// 保存是否自动润色。
#[tauri::command]
pub fn ui_save_pipeline(polish: bool) -> Result<Bootstrap, String> {
    let mut loaded = config::load(None)?;
    loaded.value.pipeline.polish = polish;
    config::save_default(&loaded.value)?;
    ui_bootstrap()
}

/// 保存翻译任务参数（语言、分段预算、超时与重试等）。
#[tauri::command]
pub fn ui_save_task_config(
    source_language: String,
    max_chars_per_segment: usize,
    max_chars_per_batch: usize,
    recent_context_chars: usize,
    timeout_secs: u64,
    max_retries: usize,
    full_book: bool,
) -> Result<Bootstrap, String> {
    let mut loaded = config::load(None)?;
    loaded.value.language.source = source_language;
    loaded.value.segment.max_chars_per_segment = max_chars_per_segment;
    loaded.value.segment.max_chars_per_batch = max_chars_per_batch;
    loaded.value.pipeline.recent_context_chars = recent_context_chars;
    loaded.value.llm.timeout_secs = timeout_secs;
    loaded.value.llm.max_retries = max_retries;
    loaded.value.analysis.full_book = full_book;
    config::save_default(&loaded.value)?;
    ui_bootstrap()
}
