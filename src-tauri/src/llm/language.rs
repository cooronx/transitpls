//! 源语言检测：从全书随机抽样三次，只有结果一致才接受。

use super::parse_json_response;
use super::TranslationClient;
use crate::model::{Document, SegmentKind};
use rand::RngExt;
use schemars::JsonSchema;
use std::time::Duration;

/// 抽样次数与每次抽样的字符数。
const SAMPLE_COUNT: usize = 3;
const SAMPLE_CHARS: usize = 1_000;

#[derive(Debug, serde::Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LanguageResponse {
    language: String,
}

/// 从正文段落中随机抽取若干段文本用于语言检测。
pub fn sample_language_texts(document: &Document) -> Result<Vec<String>, String> {
    let mut corpus = document
        .chapters
        .iter()
        .flat_map(|chapter| chapter.segments.iter())
        .filter(|segment| matches!(segment.kind, SegmentKind::Paragraph | SegmentKind::Quote))
        .map(|segment| segment.source.trim())
        .filter(|source| !source.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if corpus.is_empty() {
        // 只有标题等特殊段落时退回使用全部文本。
        corpus = document
            .chapters
            .iter()
            .flat_map(|chapter| chapter.segments.iter())
            .map(|segment| segment.source.trim())
            .filter(|source| !source.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
    }
    let chars = corpus.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return Err("cannot detect language from an empty document".to_string());
    }
    let sample_len = chars.len().min(SAMPLE_CHARS);
    let max_start = chars.len() - sample_len;
    let mut rng = rand::rng();
    Ok((0..SAMPLE_COUNT)
        .map(|_| {
            let start = if max_start == 0 {
                0
            } else {
                rng.random_range(0..=max_start)
            };
            chars[start..start + sample_len].iter().collect()
        })
        .collect())
}

/// 校验模型返回的语言代码必须是两位 ISO 639-1。
pub fn validate_language_response(raw: &str) -> Result<String, String> {
    let response: LanguageResponse = parse_json_response(raw)
        .map_err(|error| format!("language response is not valid JSON: {error}"))?;
    let language = response.language.trim().to_ascii_lowercase();
    if language.len() != 2 || !language.bytes().all(|byte| byte.is_ascii_alphabetic()) {
        return Err(format!(
            "language response must contain an ISO 639-1 code, got '{language}'"
        ));
    }
    Ok(language)
}

/// 抽样检测源语言；任一样本失败或样本结论不一致都会报错。
pub async fn detect_source_language<C: TranslationClient + ?Sized>(
    client: &C,
    document: &Document,
    max_retries: usize,
) -> Result<String, String> {
    let samples = sample_language_texts(document)?;
    let system = "You are a language identification classifier. Identify the primary natural language of the provided text. Return only valid JSON in the exact form {\"language\":\"<ISO 639-1>\"}. Do not translate or explain.";
    let schema = crate::schema::response_schema::<LanguageResponse>();
    let mut detected = Vec::with_capacity(samples.len());
    for (index, sample) in samples.iter().enumerate() {
        let mut language = None;
        let mut last_error = String::new();
        for attempt in 0..=max_retries {
            match client
                .complete_attempt(system, sample, attempt, Some(schema.clone()))
                .await
            {
                Ok(output) => match validate_language_response(&output.text) {
                    Ok(value) => {
                        language = Some(value);
                        break;
                    }
                    Err(error) => last_error = error,
                },
                Err(error) => last_error = error,
            }
            if attempt < max_retries {
                tokio::time::sleep(Duration::from_secs(1_u64 << attempt.min(6))).await;
            }
        }
        let language = language.ok_or_else(|| {
            format!(
                "language detection sample {} failed after {max_retries} retries: {last_error}",
                index + 1
            )
        })?;
        detected.push(language);
    }
    let Some(first) = detected.first() else {
        return Err("language detection produced no samples".to_string());
    };
    if detected.iter().all(|language| language == first) {
        Ok(first.clone())
    } else {
        Err(format!(
            "language detection failed: samples disagree ({})",
            detected.join(", ")
        ))
    }
}
