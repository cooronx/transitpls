use crate::config::LlmConfig;
use crate::model::{Document, Segment, SegmentKind};
use crate::terms::Term;
use async_trait::async_trait;
use rand::RngExt;
use rig_core::client::CompletionClient;
use rig_core::completion::{AssistantContent, CompletionRequestBuilder, Message, Usage};
use rig_core::http_client::ReqwestClient;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::time::Duration;

pub const LANGUAGE_SAMPLE_COUNT: usize = 3;
pub const LANGUAGE_SAMPLE_CHARS: usize = 1_000;

#[async_trait]
pub trait TranslationClient: Send + Sync {
    async fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionOutput, String>;
}

#[derive(Debug, Clone)]
pub struct CompletionOutput {
    pub text: String,
    pub usage: Usage,
}

pub struct MockClient;

#[async_trait]
impl TranslationClient for MockClient {
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
            let chapter = request["chapter"].as_u64().unwrap_or_default();
            let terms = if source.contains("Alice") {
                vec![serde_json::json!({
                    "source": "Alice",
                    "target": "爱丽丝",
                    "reading": null,
                    "type": "person",
                    "gender": null,
                    "aliases": [],
                    "first_chapter": chapter,
                    "note": "stable mock extraction",
                    "status": "ok"
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
                    "translations": request.segments.into_iter().map(|segment| serde_json::json!({
                        "number": segment.number,
                        "id": segment.id,
                        "translation": format!("[mock polished zh-CN] {}", segment.translation),
                    })).collect::<Vec<_>>()
                })
                .to_string(),
            ));
        }
        if system_prompt.contains("TASK:TITLE_TRANSLATION") {
            let request: BatchPrompt = serde_json::from_str(user_prompt)
                .map_err(|error| format!("mock client received invalid title prompt: {error}"))?;
            return Ok(mock_output(
                serde_json::json!({
                    "translations": request.segments.into_iter().map(|segment| serde_json::json!({
                        "number": segment.number,
                        "id": segment.id,
                        "translation": format!("[mock title zh-CN] {}", segment.source),
                    })).collect::<Vec<_>>()
                })
                .to_string(),
            ));
        }
        let request: BatchPrompt = serde_json::from_str(user_prompt)
            .map_err(|error| format!("mock client received invalid prompt: {error}"))?;
        Ok(mock_output(
            serde_json::json!({
                "translations": request.segments.into_iter().map(|segment| serde_json::json!({
                    "number": segment.number,
                    "id": segment.id,
                    "translation": format!("[mock zh-CN] {}", segment.source),
                })).collect::<Vec<_>>()
            })
            .to_string(),
        ))
    }
}

fn mock_output(text: String) -> CompletionOutput {
    CompletionOutput {
        text,
        usage: Usage::default(),
    }
}

enum RigModel {
    OpenAiChat(rig_core::providers::openai::completion::CompletionModel<ReqwestClient>),
    OpenAiResponses(
        rig_core::providers::openai::responses_api::ResponsesCompletionModel<ReqwestClient>,
    ),
    Anthropic(rig_core::providers::anthropic::completion::CompletionModel<ReqwestClient>),
}

pub struct RigClient {
    model: RigModel,
}

pub struct RecordingClient {
    inner: Box<dyn TranslationClient>,
    recorder: crate::usage::UsageRecorder,
}

impl RecordingClient {
    pub fn new(inner: Box<dyn TranslationClient>, recorder: crate::usage::UsageRecorder) -> Self {
        Self { inner, recorder }
    }
}

#[async_trait]
impl TranslationClient for RecordingClient {
    async fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionOutput, String> {
        let output = self.inner.complete(system_prompt, user_prompt).await?;
        self.recorder
            .record(stage_from_prompt(system_prompt), output.usage)?;
        Ok(output)
    }
}

fn stage_from_prompt(system_prompt: &str) -> &'static str {
    if system_prompt.contains("language identification") {
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

impl RigClient {
    pub fn from_config(config: &LlmConfig) -> Result<Self, String> {
        Self::from_config_with_api_key(config, config.api_key()?)
    }

    pub fn from_config_with_api_key(
        config: &LlmConfig,
        api_key: impl Into<String>,
    ) -> Result<Self, String> {
        let http_client = ReqwestClient::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|error| format!("failed to build HTTP client: {error}"))?;
        let provider = config.provider.to_ascii_lowercase();
        let api_key = api_key.into();
        let model = match provider.as_str() {
            "openai-chat" => {
                let base_url = normalize_openai_url(config.base_url.as_deref(), "chat/completions");
                let client = rig_core::providers::openai::CompletionsClient::builder()
                    .api_key(api_key.clone())
                    .base_url(base_url)
                    .http_client(http_client)
                    .build()
                    .map_err(|error| format!("failed to configure OpenAI Chat client: {error}"))?;
                RigModel::OpenAiChat(client.completion_model(config.model.clone()))
            }
            "openai-responses" => {
                let base_url = normalize_openai_url(config.base_url.as_deref(), "responses");
                let client = rig_core::providers::openai::Client::builder()
                    .api_key(api_key.clone())
                    .base_url(base_url)
                    .http_client(http_client)
                    .build()
                    .map_err(|error| {
                        format!("failed to configure OpenAI Responses client: {error}")
                    })?;
                RigModel::OpenAiResponses(client.completion_model(config.model.clone()))
            }
            "anthropic" => {
                let base_url = config
                    .base_url
                    .as_deref()
                    .unwrap_or("https://api.anthropic.com")
                    .to_string();
                let client = rig_core::providers::anthropic::Client::builder()
                    .api_key(api_key)
                    .base_url(base_url)
                    .http_client(http_client)
                    .build()
                    .map_err(|error| format!("failed to configure Anthropic client: {error}"))?;
                RigModel::Anthropic(client.completion_model(config.model.clone()))
            }
            _ => {
                return Err(format!(
                "unsupported LLM provider '{}'; use openai-chat, openai-responses, or anthropic",
                config.provider
            ))
            }
        };
        Ok(Self { model })
    }
}

#[async_trait]
impl TranslationClient for RigClient {
    async fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionOutput, String> {
        let response = match &self.model {
            RigModel::OpenAiChat(model) => {
                CompletionRequestBuilder::new(model.clone(), Message::user(user_prompt))
                    .preamble(system_prompt.to_string())
                    .max_tokens(8_192)
                    .temperature(0.1)
                    .send()
                    .await
            }
            RigModel::OpenAiResponses(model) => {
                CompletionRequestBuilder::new(model.clone(), Message::user(user_prompt))
                    .preamble(system_prompt.to_string())
                    .max_tokens(8_192)
                    .temperature(0.1)
                    .send()
                    .await
            }
            RigModel::Anthropic(model) => {
                CompletionRequestBuilder::new(model.clone(), Message::user(user_prompt))
                    .preamble(system_prompt.to_string())
                    .max_tokens(8_192)
                    .temperature(0.1)
                    .send()
                    .await
            }
        }
        .map_err(|error| format!("LLM request failed: {error}"))?;
        let usage = response.usage;
        let text = response
            .choice
            .into_iter()
            .filter_map(|content| match content {
                AssistantContent::Text(value) => Some(value.text),
                _ => None,
            })
            .collect::<String>();
        Ok(CompletionOutput { text, usage })
    }
}

#[derive(Debug, serde::Deserialize)]
struct BatchPrompt {
    segments: Vec<PromptSegment>,
}

#[derive(Debug, serde::Deserialize)]
struct PromptSegment {
    number: usize,
    id: String,
    source: String,
}

#[derive(Debug, serde::Deserialize)]
struct PolishPrompt {
    segments: Vec<PolishPromptSegment>,
}

#[derive(Debug, serde::Deserialize)]
struct PolishPromptSegment {
    number: usize,
    id: String,
    translation: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LanguageResponse {
    language: String,
}

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
    let sample_len = chars.len().min(LANGUAGE_SAMPLE_CHARS);
    let max_start = chars.len() - sample_len;
    let mut rng = rand::rng();
    Ok((0..LANGUAGE_SAMPLE_COUNT)
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

pub async fn detect_source_language<C: TranslationClient + ?Sized>(
    client: &C,
    document: &Document,
    max_retries: usize,
) -> Result<String, String> {
    let samples = sample_language_texts(document)?;
    let system = "You are a language identification classifier. Identify the primary natural language of the provided text. Return only valid JSON in the exact form {\"language\":\"<ISO 639-1>\"}. Do not translate or explain.";
    let mut detected = Vec::with_capacity(samples.len());
    for (index, sample) in samples.iter().enumerate() {
        let mut language = None;
        let mut last_error = String::new();
        for attempt in 0..=max_retries {
            match client.complete(system, sample).await {
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

#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct RecentTarget {
    pub chapter_id: String,
    pub segment_id: String,
    pub target: String,
}

pub struct TranslationContext<'a> {
    pub style_guide: &'a [String],
    pub book_synopsis: Option<&'a str>,
    pub chapter_digest: Option<&'a str>,
    pub terms: &'a [Term],
    pub recent_targets: &'a [RecentTarget],
}

#[derive(Serialize)]
struct TranslationPrompt<'a> {
    style: &'a [String],
    book_synopsis: Option<&'a str>,
    chapter_digest: Option<&'a str>,
    terms: &'a [Term],
    recent_targets: &'a [RecentTarget],
    segments: Vec<NumberedSource<'a>>,
}

#[derive(Serialize)]
struct NumberedSource<'a> {
    number: usize,
    id: &'a str,
    source: &'a str,
}

pub fn build_prompts(
    segments: &[Segment],
    source_language: &str,
    target_language: &str,
    context: &TranslationContext<'_>,
) -> (String, String) {
    let system = format!(
        "TASK:TRANSLATION You are a professional literary translator. Translate from {source_language} to {target_language}. Apply the context sections in their provided order. Preserve meaning, tone, formatting markers, and paragraph boundaries. Resolved terms are authoritative. Return only valid JSON in the exact form {{\"translations\":[{{\"number\":1,\"id\":\"segment-id\",\"translation\":\"...\"}}]}}. Keep translations in numbered input order and never omit an item."
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

pub fn validate_response(raw: &str, expected_ids: &[String]) -> Result<Vec<String>, String> {
    let response: TranslationResponse = parse_json_response(raw)
        .map_err(|error| format!("LLM response is not valid JSON: {error}"))?;
    if response.translations.len() != expected_ids.len() {
        return Err(format!(
            "LLM returned {} translations; expected {}",
            response.translations.len(),
            expected_ids.len()
        ));
    }
    let mut translations = Vec::with_capacity(response.translations.len());
    for (index, item) in response.translations.into_iter().enumerate() {
        if item.number != index + 1 {
            return Err(format!(
                "translation {index} has number {}; expected {}",
                item.number,
                index + 1
            ));
        }
        if item.id != expected_ids[index] {
            return Err(format!(
                "translation {index} has id '{}'; expected '{}'",
                item.id, expected_ids[index]
            ));
        }
        if item.translation.trim().is_empty() {
            return Err(format!("translation {index} is empty"));
        }
        translations.push(item.translation);
    }
    Ok(translations)
}

#[derive(Debug, serde::Deserialize)]
struct TranslationResponse {
    translations: Vec<TranslationItem>,
}

#[derive(Debug, serde::Deserialize)]
struct TranslationItem {
    number: usize,
    id: String,
    translation: String,
}

fn normalize_openai_url(base_url: Option<&str>, suffix: &str) -> String {
    let value = base_url
        .unwrap_or("https://api.openai.com/v1")
        .trim_end_matches('/');
    for ending in ["/chat/completions", "/responses", "/v1"] {
        if let Some(stripped) = value.strip_suffix(ending) {
            return if ending == "/v1" {
                stripped.to_string() + "/v1"
            } else {
                stripped.to_string()
            };
        }
    }
    if let Some(stripped) = value.strip_suffix(suffix) {
        stripped.trim_end_matches('/').to_string()
    } else {
        value.to_string()
    }
}

pub async fn translate_batch<C: TranslationClient + ?Sized>(
    client: &C,
    segments: &[Segment],
    source_language: &str,
    target_language: &str,
    context: &TranslationContext<'_>,
    max_retries: usize,
) -> Result<Vec<String>, String> {
    let expected_ids = segments
        .iter()
        .map(|segment| segment.id.clone())
        .collect::<Vec<_>>();
    let (system, user) = build_prompts(segments, source_language, target_language, context);
    let mut last_error = String::new();
    for attempt in 0..=max_retries {
        match client.complete(&system, &user).await {
            Ok(output) => match validate_response(&output.text, &expected_ids) {
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

#[derive(Serialize)]
struct PolishRequest<'a> {
    style: &'a [String],
    book_synopsis: Option<&'a str>,
    chapter_digest: Option<&'a str>,
    terms: &'a [Term],
    recent_targets: &'a [RecentTarget],
    segments: Vec<PolishSource<'a>>,
}

#[derive(Serialize)]
struct PolishSource<'a> {
    number: usize,
    id: &'a str,
    source: &'a str,
    translation: &'a str,
}

pub async fn polish_batch<C: TranslationClient + ?Sized>(
    client: &C,
    segments: &[Segment],
    context: &TranslationContext<'_>,
    max_retries: usize,
) -> Result<Vec<String>, String> {
    let expected_ids = segments
        .iter()
        .map(|segment| segment.id.clone())
        .collect::<Vec<_>>();
    let system = "TASK:POLISH Polish the draft Simplified Chinese translations while preserving meaning, paragraph boundaries, and authoritative resolved terminology. Return only valid JSON in the exact form {\"translations\":[{\"number\":1,\"id\":\"segment-id\",\"translation\":\"...\"}]}. Keep items in numbered input order and never omit an item.";
    let user = serde_json::to_string(&PolishRequest {
        style: context.style_guide,
        book_synopsis: context.book_synopsis,
        chapter_digest: context.chapter_digest,
        terms: context.terms,
        recent_targets: context.recent_targets,
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
    call_numbered_batch(client, system, &user, &expected_ids, max_retries, "polish").await
}

pub async fn translate_titles<C: TranslationClient + ?Sized>(
    client: &C,
    titles: &[Segment],
    source_language: &str,
    target_language: &str,
    style_guide: &[String],
    max_retries: usize,
) -> Result<Vec<String>, String> {
    let expected_ids = titles
        .iter()
        .map(|title| title.id.clone())
        .collect::<Vec<_>>();
    let system = format!(
        "TASK:TITLE_TRANSLATION Translate chapter and table-of-contents titles from {source_language} to {target_language}. Follow the style guide and keep titles concise. Return only valid JSON in the exact form {{\"translations\":[{{\"number\":1,\"id\":\"chapter-id\",\"translation\":\"...\"}}]}}. Keep items in numbered input order and never omit an item."
    );
    let user = serde_json::to_string(&TranslationPrompt {
        style: style_guide,
        book_synopsis: None,
        chapter_digest: None,
        terms: &[],
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
        &expected_ids,
        max_retries,
        "title batch",
    )
    .await
}

async fn call_numbered_batch<C: TranslationClient + ?Sized>(
    client: &C,
    system: &str,
    user: &str,
    expected_ids: &[String],
    max_retries: usize,
    label: &str,
) -> Result<Vec<String>, String> {
    let mut last_error = String::new();
    for attempt in 0..=max_retries {
        match client.complete(system, user).await {
            Ok(output) => match validate_response(&output.text, expected_ids) {
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

#[cfg(test)]
mod tests {
    use super::{
        detect_source_language, sample_language_texts, validate_language_response,
        validate_response, CompletionOutput, TranslationClient,
    };
    use crate::model::{Chapter, Document, DocumentMetadata, ItemStatus, Segment, SegmentKind};
    use crate::terms::{Term, TermStatus};
    use async_trait::async_trait;
    use std::sync::{Arc, Mutex};

    fn document_with_source(source: &str) -> Document {
        Document {
            metadata: DocumentMetadata {
                title: "Test".to_string(),
                source_language: "auto".to_string(),
                target_language: "zh-CN".to_string(),
                source_format: "txt".to_string(),
            },
            chapters: vec![Chapter {
                id: "chapter-1".to_string(),
                title: "Chapter 1".to_string(),
                target_title: None,
                status: ItemStatus::Pending,
                meta: serde_json::json!({}),
                segments: vec![Segment {
                    id: "segment-1".to_string(),
                    ordinal: 0,
                    source: source.to_string(),
                    target: None,
                    target_before_polish: None,
                    kind: SegmentKind::Paragraph,
                    status: ItemStatus::Pending,
                    source_hash: "hash".to_string(),
                    meta: serde_json::json!({}),
                }],
            }],
        }
    }

    #[test]
    fn translation_prompt_includes_relevant_terms() {
        let segment = document_with_source("Alice arrived.").chapters[0].segments[0].clone();
        let term = Term {
            source: "Alice".to_string(),
            target: "爱丽丝".to_string(),
            reading: None,
            term_type: "person".to_string(),
            gender: None,
            aliases: Vec::new(),
            first_chapter: 0,
            note: None,
            status: TermStatus::Resolved,
        };
        let terms = [term];
        let context = super::TranslationContext {
            style_guide: &["Keep the voice".to_string()],
            book_synopsis: Some("Book synopsis"),
            chapter_digest: Some("Chapter digest"),
            terms: &terms,
            recent_targets: &[super::RecentTarget {
                chapter_id: "chapter-0".to_string(),
                segment_id: "segment-0".to_string(),
                target: "最近译文".to_string(),
            }],
        };
        let (_, user) = super::build_prompts(&[segment], "en", "zh-CN", &context);
        let value: serde_json::Value =
            serde_json::from_str(&user).expect("prompt should be valid JSON");
        assert_eq!(value["terms"][0]["target"], "爱丽丝");
        assert_eq!(value["terms"][0]["status"], "resolved");
        assert_eq!(value["segments"][0]["number"], 1);
        let positions = [
            "\"style\"",
            "\"book_synopsis\"",
            "\"chapter_digest\"",
            "\"terms\"",
            "\"recent_targets\"",
            "\"segments\"",
        ]
        .map(|field| user.find(field).expect("prompt field should exist"));
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
    }

    struct SequenceClient {
        responses: Arc<Mutex<Vec<Result<String, String>>>>,
    }

    impl SequenceClient {
        fn new(responses: Vec<Result<String, String>>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses)),
            }
        }
    }

    #[async_trait]
    impl TranslationClient for SequenceClient {
        async fn complete(
            &self,
            _system_prompt: &str,
            _user_prompt: &str,
        ) -> Result<CompletionOutput, String> {
            self.responses
                .lock()
                .expect("sequence client mutex")
                .remove(0)
                .map(super::mock_output)
        }
    }

    #[test]
    fn validates_order_and_rejects_empty_translation() {
        let ids = vec!["a".to_string(), "b".to_string()];
        let valid = r#"{"translations":[{"number":1,"id":"a","translation":"甲"},{"number":2,"id":"b","translation":"乙"}]}"#;
        assert_eq!(
            validate_response(valid, &ids).expect("valid response"),
            vec!["甲", "乙"]
        );
        let empty = r#"{"translations":[{"number":1,"id":"a","translation":" "},{"number":2,"id":"b","translation":"乙"}]}"#;
        assert!(validate_response(empty, &ids).is_err());
        let fenced = format!("```json\n{valid}\n```");
        assert_eq!(
            validate_response(&fenced, &ids).expect("fenced JSON should be accepted"),
            vec!["甲", "乙"]
        );
        assert!(validate_response(&format!("Result:\n{valid}"), &ids).is_err());
    }

    #[test]
    fn samples_three_random_excerpts_with_unicode_character_limit() {
        let document = document_with_source(&"あ".repeat(2_500));
        let samples = sample_language_texts(&document).expect("samples");
        assert_eq!(samples.len(), 3);
        assert!(samples.iter().all(|sample| sample.chars().count() == 1_000));
    }

    #[tokio::test]
    async fn accepts_language_only_when_all_three_samples_agree() {
        let client = SequenceClient::new(vec![
            Ok(r#"{"language":"ja"}"#.to_string()),
            Ok(r#"{"language":"ja"}"#.to_string()),
            Ok(r#"{"language":"JA"}"#.to_string()),
        ]);
        let document = document_with_source(&"日本語の文章です。".repeat(150));
        assert_eq!(
            detect_source_language(&client, &document, 0)
                .await
                .expect("language detection"),
            "ja"
        );
    }

    #[tokio::test]
    async fn rejects_language_when_samples_disagree() {
        let client = SequenceClient::new(vec![
            Ok(r#"{"language":"ja"}"#.to_string()),
            Ok(r#"{"language":"en"}"#.to_string()),
            Ok(r#"{"language":"ja"}"#.to_string()),
        ]);
        let document = document_with_source(&"sample text ".repeat(200));
        let error = detect_source_language(&client, &document, 0)
            .await
            .expect_err("disagreement must fail");
        assert!(error.contains("samples disagree"));
    }

    #[test]
    fn validates_strict_iso_language_response() {
        assert_eq!(
            validate_language_response(r#"{"language":" JA "}"#).expect("valid code"),
            "ja"
        );
        assert!(validate_language_response(r#"{"language":"jpn"}"#).is_err());
        assert!(validate_language_response(r#"{"language":"ja","extra":true}"#).is_err());
    }
}
