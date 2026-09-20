//! Mock 客户端：按任务标记返回固定译文，供离线测试与界面演示使用。

use super::prompts::{BatchPrompt, PolishPrompt};
use super::{CompletionOutput, TranslationClient};
use async_trait::async_trait;
use rig_core::completion::Usage;

/// 按任务标记返回固定译文，供离线测试与界面演示使用。
pub struct MockClient;

#[async_trait]
impl TranslationClient for MockClient {
    fn model_name(&self) -> Option<&str> {
        Some("mock")
    }

    async fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionOutput, String> {
        if system_prompt.contains("language identification") {
            return Ok(mock_output(
                serde_json::json!({
                    "language": mock_language(user_prompt),
                })
                .to_string(),
            ));
        }
        if system_prompt.contains("TASK:BOOK_STYLE_ANALYSIS") {
            return Ok(mock_output(
                serde_json::json!({
                    "genre": "mock fiction",
                    "tone": "consistent mock tone",
                    "style_guide": [
                        "Use natural Simplified Chinese",
                        "Keep names and forms of address consistent"
                    ],
                    "narration": "mock third-person narration",
                    "pacing": "balanced",
                    "register": "neutral",
                    "dialogue_style": "concise",
                    "rhetoric": "plain",
                    "characters": [{
                        "source": "Alice",
                        "target": "爱丽丝",
                        "reading": null,
                        "type": "person",
                        "gender": null,
                        "aliases": [],
                        "first_chapter": 0,
                        "note": "stable mock character"
                    }],
                    "terms": [{
                        "source": "city",
                        "target": "城市",
                        "reading": null,
                        "type": "place",
                        "gender": null,
                        "aliases": [],
                        "first_chapter": 0,
                        "note": "stable mock term"
                    }],
                    "book_synopsis": null
                })
                .to_string(),
            ));
        }
        if system_prompt.contains("TASK:CHAPTER_DIGEST") {
            let request: serde_json::Value = serde_json::from_str(user_prompt)
                .map_err(|error| format!("mock client received invalid digest prompt: {error}"))?;
            let title = request["title"].as_str().unwrap_or("Untitled");
            return Ok(mock_output(
                serde_json::json!({
                    "source_digest": format!("Mock digest for {title}"),
                })
                .to_string(),
            ));
        }
        if system_prompt.contains("TASK:BOOK_SYNOPSIS") {
            return Ok(mock_output(
                serde_json::json!({
                    "book_synopsis": "Stable mock whole-book synopsis"
                })
                .to_string(),
            ));
        }
        if system_prompt.contains("TASK:TERM_EXTRACTION") {
            let request: serde_json::Value =
                serde_json::from_str(user_prompt).map_err(|error| {
                    format!("mock client received invalid term extraction prompt: {error}")
                })?;
            let source = request["source"].as_str().unwrap_or_default();
            let terms = if source.contains("Alice") {
                vec![serde_json::json!({
                    "source": "Alice",
                    "target": "爱丽丝",
                    "reading": null,
                    "type": "person",
                    "gender": null,
                    "aliases": [],
                    "note": "stable mock extraction"
                })]
            } else {
                Vec::new()
            };
            return Ok(mock_output(
                serde_json::json!({ "terms": terms }).to_string(),
            ));
        }
        if system_prompt.contains("TASK:POLISH") {
            let request: PolishPrompt = serde_json::from_str(user_prompt)
                .map_err(|error| format!("mock client received invalid polish prompt: {error}"))?;
            return Ok(mock_output(
                serde_json::json!({
                    "translations": request.segments.into_iter()
                        .map(|segment| format!("[mock polished zh-CN] {}", segment.translation))
                        .collect::<Vec<_>>()
                })
                .to_string(),
            ));
        }
        if system_prompt.contains("TASK:TITLE_TRANSLATION") {
            let request: BatchPrompt = serde_json::from_str(user_prompt)
                .map_err(|error| format!("mock client received invalid title prompt: {error}"))?;
            return Ok(mock_output(
                serde_json::json!({
                    "translations": request.segments.into_iter()
                        .map(|segment| format!("[mock title zh-CN] {}", segment.source))
                        .collect::<Vec<_>>()
                })
                .to_string(),
            ));
        }
        let request: BatchPrompt = serde_json::from_str(user_prompt)
            .map_err(|error| format!("mock client received invalid prompt: {error}"))?;
        Ok(mock_output(
            serde_json::json!({
                "translations": request.segments.into_iter()
                    .map(|segment| format!("[mock zh-CN] {}", segment.source))
                    .collect::<Vec<_>>()
            })
            .to_string(),
        ))
    }
}

/// 包装 mock 文本，附带空的用量信息。
pub(super) fn mock_output(text: String) -> CompletionOutput {
    CompletionOutput {
        text,
        usage: Usage::default(),
    }
}

/// 按字符范围粗略判断语言，仅用于 mock 客户端。
fn mock_language(text: &str) -> &'static str {
    if text.chars().any(|value| {
        ('\u{3040}'..='\u{30ff}').contains(&value) || ('\u{ff66}'..='\u{ff9d}').contains(&value)
    }) {
        "ja"
    } else if text
        .chars()
        .any(|value| ('\u{4e00}'..='\u{9fff}').contains(&value))
    {
        "zh"
    } else {
        "en"
    }
}
