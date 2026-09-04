use crate::model::Segment;
use async_trait::async_trait;
use rig_core::client::CompletionClient;
use rig_core::completion::{AssistantContent, CompletionRequestBuilder, Message};
use rig_core::http_client::ReqwestClient;
use std::time::Duration;

pub const DEFAULT_TIMEOUT_SECS: u64 = 60;
pub const DEFAULT_MAX_RETRIES: usize = 3;

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AppConfig {
    pub llm: LlmConfig,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct LlmConfig {
    pub provider: String,
    pub model: String,
    pub base_url: Option<String>,
    pub api_key: String,
}

#[async_trait]
pub trait TranslationClient: Send + Sync {
    async fn complete(&self, system_prompt: &str, user_prompt: &str) -> Result<String, String>;
}

pub struct MockClient;

#[async_trait]
impl TranslationClient for MockClient {
    async fn complete(&self, _system_prompt: &str, user_prompt: &str) -> Result<String, String> {
        let request: BatchPrompt = serde_json::from_str(user_prompt)
            .map_err(|error| format!("mock client received invalid prompt: {error}"))?;
        Ok(serde_json::json!({
            "translations": request.segments.into_iter().map(|segment| serde_json::json!({
                "id": segment.id,
                "translation": format!("[mock zh-CN] {}", segment.source),
            })).collect::<Vec<_>>()
        })
        .to_string())
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

impl RigClient {
    pub fn from_config(config: &LlmConfig) -> Result<Self, String> {
        let http_client = ReqwestClient::builder()
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()
            .map_err(|error| format!("failed to build HTTP client: {error}"))?;
        let provider = config.provider.to_ascii_lowercase();
        let model = match provider.as_str() {
            "openai-chat" => {
                let base_url = normalize_openai_url(config.base_url.as_deref(), "chat/completions");
                let client = rig_core::providers::openai::CompletionsClient::builder()
                    .api_key(config.api_key.clone())
                    .base_url(base_url)
                    .http_client(http_client)
                    .build()
                    .map_err(|error| format!("failed to configure OpenAI Chat client: {error}"))?;
                RigModel::OpenAiChat(client.completion_model(config.model.clone()))
            }
            "openai-responses" => {
                let base_url = normalize_openai_url(config.base_url.as_deref(), "responses");
                let client = rig_core::providers::openai::Client::builder()
                    .api_key(config.api_key.clone())
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
                    .api_key(config.api_key.clone())
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
    async fn complete(&self, system_prompt: &str, user_prompt: &str) -> Result<String, String> {
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
        let text = response
            .choice
            .into_iter()
            .filter_map(|content| match content {
                AssistantContent::Text(value) => Some(value.text),
                _ => None,
            })
            .collect::<String>();
        if text.trim().is_empty() {
            Err("LLM returned an empty completion".to_string())
        } else {
            Ok(text)
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct BatchPrompt {
    segments: Vec<PromptSegment>,
}

#[derive(Debug, serde::Deserialize)]
struct PromptSegment {
    id: String,
    source: String,
}

pub fn build_prompts(
    segments: &[Segment],
    source_language: &str,
    target_language: &str,
) -> (String, String) {
    let system = format!(
        "You are a professional literary translator. Translate from {source_language} to {target_language}. Preserve meaning, tone, formatting markers, and paragraph boundaries. Return only valid JSON in the exact form {{\"translations\":[{{\"id\":\"segment-id\",\"translation\":\"...\"}}]}}. Keep translations in input order and never omit an item."
    );
    let user = serde_json::json!({
        "segments": segments.iter().map(|segment| serde_json::json!({
            "id": segment.id,
            "source": segment.source,
        })).collect::<Vec<_>>()
    })
    .to_string();
    (system, user)
}

pub fn validate_response(raw: &str, expected_ids: &[String]) -> Result<Vec<String>, String> {
    let response: TranslationResponse = serde_json::from_str(raw)
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

pub fn load_config(path: &std::path::Path) -> Result<AppConfig, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read config {}: {error}", path.display()))?;
    toml::from_str(&text).map_err(|error| format!("invalid TOML config: {error}"))
}

pub async fn translate_batch<C: TranslationClient + ?Sized>(
    client: &C,
    segments: &[Segment],
    source_language: &str,
    target_language: &str,
) -> Result<Vec<String>, String> {
    let expected_ids = segments
        .iter()
        .map(|segment| segment.id.clone())
        .collect::<Vec<_>>();
    let (system, user) = build_prompts(segments, source_language, target_language);
    let mut last_error = String::new();
    for attempt in 0..=DEFAULT_MAX_RETRIES {
        match client.complete(&system, &user).await {
            Ok(raw) => match validate_response(&raw, &expected_ids) {
                Ok(translations) => return Ok(translations),
                Err(error) => last_error = error,
            },
            Err(error) => last_error = error,
        }
        if attempt < DEFAULT_MAX_RETRIES {
            tokio::time::sleep(Duration::from_secs(1_u64 << attempt)).await;
        }
    }
    Err(format!(
        "batch failed after {} retries: {last_error}",
        DEFAULT_MAX_RETRIES
    ))
}

#[cfg(test)]
mod tests {
    use super::validate_response;

    #[test]
    fn validates_order_and_rejects_empty_translation() {
        let ids = vec!["a".to_string(), "b".to_string()];
        let valid =
            r#"{"translations":[{"id":"a","translation":"甲"},{"id":"b","translation":"乙"}]}"#;
        assert_eq!(
            validate_response(valid, &ids).expect("valid response"),
            vec!["甲", "乙"]
        );
        let empty =
            r#"{"translations":[{"id":"a","translation":" "},{"id":"b","translation":"乙"}]}"#;
        assert!(validate_response(empty, &ids).is_err());
    }
}
