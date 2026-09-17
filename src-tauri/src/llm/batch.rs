//! 批次调用：翻译、润色与标题翻译，统一处理重试与响应校验。

use super::prompts::{
    validate_response, NumberedSource, PolishRequest, PolishSource, TranslationPrompt,
    TranslationResponse,
};
use super::{TranslationClient, TranslationContext};
use crate::model::Segment;
use crate::terms::Term;
use std::time::Duration;

/// 翻译一批段落，失败时按 `max_retries` 指数退避重试。
pub async fn translate_batch<C: TranslationClient + ?Sized>(
    client: &C,
    segments: &[Segment],
    source_language: &str,
    target_language: &str,
    context: &TranslationContext<'_>,
    max_retries: usize,
) -> Result<Vec<String>, String> {
    let (system, user) = super::build_prompts(segments, source_language, target_language, context);
    let schema = crate::schema::response_schema::<TranslationResponse>();
    let mut last_error = String::new();
    for attempt in 0..=max_retries {
        match client
            .complete_attempt(&system, &user, attempt, Some(schema.clone()))
            .await
        {
            Ok(output) => match validate_response(&output.text, segments.len()) {
                Ok(translations) => return Ok(translations),
                Err(error) => last_error = error,
            },
            Err(error) => last_error = error,
        }
        if attempt < max_retries {
            tokio::time::sleep(Duration::from_secs(1_u64 << attempt.min(6))).await;
        }
    }
    Err(format!(
        "batch failed after {} retries: {last_error}",
        max_retries
    ))
}

/// 润色一批译文：以 `target_before_polish` 为草稿，`recent_targets` 仅作一致性参考。
pub async fn polish_batch<C: TranslationClient + ?Sized>(
    client: &C,
    segments: &[Segment],
    context: &TranslationContext<'_>,
    max_retries: usize,
) -> Result<Vec<String>, String> {
    let system = "TASK:POLISH Polish the draft Simplified Chinese translations while preserving meaning, paragraph boundaries, and authoritative resolved terminology. The reference_targets section is earlier draft context for consistency only: do not translate, return, or modify it. Only the segments array is processed. Return only JSON as {\"translations\":[\"<polished text>\"]} holding exactly one polished string per input segment, in input order, and never omit an item.";
    let user = serde_json::to_string(&PolishRequest {
        style: context.style_guide,
        book_synopsis: context.book_synopsis,
        chapter_digest: context.chapter_digest,
        terms: context.terms,
        reference_targets: context.recent_targets,
        segments: segments
            .iter()
            .enumerate()
            .map(|(index, segment)| PolishSource {
                number: index + 1,
                id: &segment.id,
                source: &segment.source,
                translation: segment.target_before_polish.as_deref().unwrap_or_default(),
            })
            .collect(),
    })
    .expect("polish prompt fields are serializable");
    call_numbered_batch(client, system, &user, segments.len(), max_retries, "polish").await
}

/// 翻译章节标题，只带风格指南和术语，不引入上下文。
pub async fn translate_titles<C: TranslationClient + ?Sized>(
    client: &C,
    titles: &[Segment],
    source_language: &str,
    target_language: &str,
    style_guide: &[String],
    terms: &[Term],
    max_retries: usize,
) -> Result<Vec<String>, String> {
    let system = format!(
        "TASK:TITLE_TRANSLATION Translate chapter and table-of-contents titles from {source_language} to {target_language}. Follow the style guide and keep titles concise. Return only JSON as {{\"translations\":[\"<translated title>\"]}} holding exactly one title per input item, in input order, and never omit an item."
    );
    let user = serde_json::to_string(&TranslationPrompt {
        style: style_guide,
        book_synopsis: None,
        chapter_digest: None,
        terms,
        recent_targets: &[],
        segments: titles
            .iter()
            .enumerate()
            .map(|(index, title)| NumberedSource {
                number: index + 1,
                id: &title.id,
                source: &title.source,
            })
            .collect(),
    })
    .expect("title prompt fields are serializable");
    call_numbered_batch(
        client,
        &system,
        &user,
        titles.len(),
        max_retries,
        "title batch",
    )
    .await
}

/// 带重试和数量校验的通用批次调用。
async fn call_numbered_batch<C: TranslationClient + ?Sized>(
    client: &C,
    system: &str,
    user: &str,
    expected_count: usize,
    max_retries: usize,
    label: &str,
) -> Result<Vec<String>, String> {
    let schema = crate::schema::response_schema::<TranslationResponse>();
    let mut last_error = String::new();
    for attempt in 0..=max_retries {
        match client
            .complete_attempt(system, user, attempt, Some(schema.clone()))
            .await
        {
            Ok(output) => match validate_response(&output.text, expected_count) {
                Ok(translations) => return Ok(translations),
                Err(error) => last_error = error,
            },
            Err(error) => last_error = error,
        }
        if attempt < max_retries {
            tokio::time::sleep(Duration::from_secs(1_u64 << attempt.min(6))).await;
        }
    }
    Err(format!(
        "{label} failed after {max_retries} retries: {last_error}"
    ))
}
