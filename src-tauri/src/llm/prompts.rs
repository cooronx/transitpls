//! 提示词构建与响应校验。
//!
//! 提示词一律要求模型返回 JSON：`{"translations":[...]}`，并按输入顺序一一对应。

use super::RecentTarget;
use crate::model::Segment;
use crate::terms::Term;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::Serialize;

/// mock 客户端解析用户提示词用到的结构。
#[derive(Debug, serde::Deserialize)]
pub(super) struct BatchPrompt {
    pub(super) segments: Vec<PromptSegment>,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct PromptSegment {
    pub(super) source: String,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct PolishPrompt {
    pub(super) segments: Vec<PolishPromptSegment>,
}

#[derive(Debug, serde::Deserialize)]
pub(super) struct PolishPromptSegment {
    pub(super) translation: String,
}

/// 模型返回的批量译文，`translations` 数量必须与输入段落一致。
#[derive(Debug, serde::Deserialize, JsonSchema)]
pub(super) struct TranslationResponse {
    pub(super) translations: Vec<String>,
}

/// 翻译批次提示词的用户部分。
#[derive(Serialize)]
pub(super) struct TranslationPrompt<'a> {
    pub(super) style: &'a [String],
    pub(super) book_synopsis: Option<&'a str>,
    pub(super) chapter_digest: Option<&'a str>,
    pub(super) terms: &'a [Term],
    pub(super) recent_targets: &'a [RecentTarget],
    pub(super) segments: Vec<NumberedSource<'a>>,
}

#[derive(Serialize)]
pub(super) struct NumberedSource<'a> {
    pub(super) number: usize,
    pub(super) id: &'a str,
    pub(super) source: &'a str,
}

/// 润色批次提示词的用户部分。
#[derive(Serialize)]
pub(super) struct PolishRequest<'a> {
    pub(super) style: &'a [String],
    pub(super) book_synopsis: Option<&'a str>,
    pub(super) chapter_digest: Option<&'a str>,
    pub(super) terms: &'a [Term],
    pub(super) reference_targets: &'a [RecentTarget],
    pub(super) segments: Vec<PolishSource<'a>>,
}

#[derive(Serialize)]
pub(super) struct PolishSource<'a> {
    pub(super) number: usize,
    pub(super) id: &'a str,
    pub(super) source: &'a str,
    pub(super) translation: &'a str,
}

/// 组装翻译提示词，返回（系统提示词，用户提示词）。
pub fn build_prompts(
    segments: &[Segment],
    source_language: &str,
    target_language: &str,
    context: &super::TranslationContext<'_>,
) -> (String, String) {
    let system = format!(
        "TASK:TRANSLATION You are a professional literary translator. Translate from {source_language} to {target_language}. Apply the context sections in their provided order. Preserve meaning, tone, formatting markers, and paragraph boundaries. Resolved terms are authoritative. Return only JSON as {{\"translations\":[\"<translated text>\"]}} holding exactly one translated string per input segment, in input order, and never omit an item."
    );
    let user = serde_json::to_string(&TranslationPrompt {
        style: context.style_guide,
        book_synopsis: context.book_synopsis,
        chapter_digest: context.chapter_digest,
        terms: context.terms,
        recent_targets: context.recent_targets,
        segments: segments
            .iter()
            .enumerate()
            .map(|(index, segment)| NumberedSource {
                number: index + 1,
                id: &segment.id,
                source: &segment.source,
            })
            .collect(),
    })
    .expect("translation prompt fields are serializable");
    (system, user)
}

/// 校验批量译文的数量与空值，返回按输入顺序排列的译文。
pub fn validate_response(raw: &str, expected_count: usize) -> Result<Vec<String>, String> {
    let response: TranslationResponse = parse_json_response(raw)
        .map_err(|error| format!("LLM response is not valid JSON: {error}"))?;
    if response.translations.len() != expected_count {
        return Err(format!(
            "LLM returned {} translations; expected {expected_count}",
            response.translations.len()
        ));
    }
    for (index, translation) in response.translations.iter().enumerate() {
        if translation.trim().is_empty() {
            return Err(format!("translation {index} is empty"));
        }
    }
    Ok(response.translations)
}

/// 解析模型返回的 JSON，兼容被 ``` 代码块包裹的情况。
pub(crate) fn parse_json_response<T: DeserializeOwned>(raw: &str) -> Result<T, String> {
    let trimmed = raw.trim();
    let json = if let Some(fenced) = trimmed.strip_prefix("```json") {
        fenced
            .strip_suffix("```")
            .ok_or_else(|| "JSON code fence is not closed".to_string())?
            .trim()
    } else if let Some(fenced) = trimmed.strip_prefix("```") {
        fenced
            .strip_suffix("```")
            .ok_or_else(|| "JSON code fence is not closed".to_string())?
            .trim()
    } else {
        trimmed
    };
    serde_json::from_str(json).map_err(|error| error.to_string())
}
