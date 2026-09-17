//! LLM 访问层：定义统一的翻译客户端接口，并封装提示词、批次翻译与语言检测。

mod batch;
mod client;
mod language;
mod mock;
mod prompts;
mod recording;

#[cfg(test)]
mod tests;

use async_trait::async_trait;
use rig_core::completion::Usage;
use schemars::Schema;
use serde::Serialize;

pub use batch::{polish_batch, translate_batch, translate_titles};
pub use client::RigClient;
pub use language::{detect_source_language, sample_language_texts, validate_language_response};
pub use mock::MockClient;
pub(crate) use prompts::parse_json_response;
pub use prompts::{build_prompts, validate_response};
pub use recording::RecordingClient;

/// 模型返回空补全时的固定错误文本，用于识别可重试的解析失败。
pub(crate) const EMPTY_COMPLETION_ERROR: &str =
    "Response contained no message or tool call (empty)";

/// 翻译客户端抽象。
///
/// 默认的 `complete_attempt` 只调用 `complete`；需要重试或结构化输出的生产客户端
/// 会覆盖它，把 `schema` 转发给服务端。
#[async_trait]
pub trait TranslationClient: Send + Sync {
    /// 发起一次补全尝试。
    ///
    /// 无法请求结构化输出的客户端可以忽略 `schema`，只依赖提示词中的 JSON 要求。
    async fn complete_attempt(
        &self,
        system: &str,
        user: &str,
        _retry: usize,
        _schema: Option<Schema>,
    ) -> Result<CompletionOutput, String> {
        self.complete(system, user).await
    }

    /// 记录一次失败，默认不处理。
    fn record_failure(&self, _stage: &str, _details: serde_json::Value) -> Result<(), String> {
        Ok(())
    }

    /// 发送一次纯文本补全。
    async fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionOutput, String>;
}

/// 一次补全的文本结果与 token 用量。
#[derive(Debug, Clone)]
pub struct CompletionOutput {
    pub text: String,
    pub usage: Usage,
}

/// 最近译好的段落，作为下一批次的上下文参考。
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct RecentTarget {
    pub chapter_id: String,
    pub segment_id: String,
    pub target: String,
}

/// 翻译与润色批次共享的上下文：风格、梗概、章节摘要、术语和最近译文。
pub struct TranslationContext<'a> {
    pub style_guide: &'a [String],
    pub book_synopsis: Option<&'a str>,
    pub chapter_digest: Option<&'a str>,
    pub terms: &'a [crate::terms::Term],
    pub recent_targets: &'a [RecentTarget],
}

/// 按系统提示词中的任务标记归类调用阶段，用于用量统计和错误归类。
fn stage_from_prompt(system_prompt: &str) -> &'static str {
    if system_prompt.contains("checking whether an LLM connection") {
        "model_verification"
    } else if system_prompt.contains("language identification") {
        "language_identification"
    } else if system_prompt.contains("TASK:BOOK_STYLE_ANALYSIS") {
        "book_style_analysis"
    } else if system_prompt.contains("TASK:CHAPTER_DIGEST") {
        "chapter_digest"
    } else if system_prompt.contains("TASK:BOOK_SYNOPSIS") {
        "book_synopsis"
    } else if system_prompt.contains("TASK:TERM_EXTRACTION") {
        "term_extraction"
    } else if system_prompt.contains("TASK:POLISH") {
        "polish"
    } else if system_prompt.contains("TASK:TITLE_TRANSLATION") {
        "title_translation"
    } else {
        "translation"
    }
}
