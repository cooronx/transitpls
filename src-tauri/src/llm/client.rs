//! 生产环境 LLM 客户端：基于 rig-core 对接 OpenAI Chat/Responses 与 Anthropic 协议。

use super::{stage_from_prompt, CompletionOutput, TranslationClient, EMPTY_COMPLETION_ERROR};
use crate::config::LlmConfig;
use async_trait::async_trait;
use futures::StreamExt;
use rig_core::client::CompletionClient;
use rig_core::completion::{
    AssistantContent, CompletionError, CompletionModel, CompletionRequestBuilder,
    CompletionResponse, Message,
};
use rig_core::http_client::ReqwestClient;
use rig_core::streaming::StreamingCompletionResponse;
use schemars::Schema;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// 支持的三种服务商协议对应的 rig 模型。
enum RigModel {
    OpenAiChat(rig_core::providers::openai::completion::CompletionModel<ReqwestClient>),
    OpenAiResponses(
        rig_core::providers::openai::responses_api::ResponsesCompletionModel<ReqwestClient>,
    ),
    Anthropic(rig_core::providers::anthropic::completion::CompletionModel<ReqwestClient>),
}

/// 真实 LLM 客户端，按配置创建对应协议模型并记录请求事件。
pub struct RigClient {
    model: RigModel,
    provider: String,
    model_name: String,
    host: String,
    recorder: Option<crate::usage::UsageRecorder>,
    /// 服务端拒绝 json_schema 后置为 true，本次运行剩余请求改为提示词级 JSON。
    structured_output_disabled: AtomicBool,
}

/// 一次流式补全请求；调用方负责把流读干。
///
/// 使用流式而非一元请求，可以避免部分服务商在长批次上中断连接。
async fn stream_completion<M: CompletionModel>(
    model: M,
    system_prompt: &str,
    user_prompt: &str,
    schema: Option<Schema>,
) -> Result<StreamingCompletionResponse, CompletionError> {
    CompletionRequestBuilder::new(model, Message::user(user_prompt))
        .preamble(system_prompt.to_string())
        .max_tokens(8_192)
        .temperature(0.1)
        .output_schema_opt(schema)
        .stream()
        .await
}

enum RequestError {
    Provider(CompletionError),
    /// 错误文本已经整理好（空补全、事件记录失败等），可直接返回给调用方。
    Rendered(String),
}

impl RigClient {
    pub fn with_recorder(mut self, recorder: crate::usage::UsageRecorder) -> Self {
        self.recorder = Some(recorder);
        self
    }

    pub fn from_config(config: &LlmConfig) -> Result<Self, String> {
        Self::from_config_with_api_key(config, config.api_key()?)
    }

    pub fn from_config_with_api_key(
        config: &LlmConfig,
        api_key: impl Into<String>,
    ) -> Result<Self, String> {
        let config = config.normalized()?;
        let api_key = api_key.into();
        if api_key.trim().is_empty() && !config.allows_empty_key() {
            return Err(format!("configuration_failed provider={} model={} stage=configuration: API key is required for this endpoint", config.provider, config.model));
        }
        let http_client = ReqwestClient::builder()
            .timeout(Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|_| {
                "configuration_failed stage=configuration: failed to build HTTP client".to_string()
            })?;
        let provider = config.provider.clone();
        let base_url = config.base_url.as_deref().expect("normalized URL");
        let host = url::Url::parse(base_url)
            .expect("validated URL")
            .host_str()
            .unwrap_or_default()
            .to_string();
        let model_name = if api_key.is_empty() {
            config.model.clone()
        } else {
            config.model.replace(&api_key, "[redacted]")
        };
        let model = match provider.as_str() {
            "openai-chat" | "openai-compatible" => {
                let client = if api_key.is_empty() {
                    // 复用 Rig 的免认证传输，同时保留 Chat 协议。
                    rig_core::providers::ollama::Client::builder()
                        .api_key(rig_core::client::Nothing)
                        .base_url(base_url)
                        .http_client(http_client)
                        .build()
                        .map(|client| client.with_ext(rig_core::providers::openai::OpenAICompletionsExt::default()))
                } else {
                    rig_core::providers::openai::CompletionsClient::builder()
                        .api_key(api_key.clone())
                        .base_url(base_url)
                        .http_client(http_client)
                        .build()
                }
                    .map_err(|_| "configuration_failed stage=configuration: invalid Chat client settings or API key header".to_string())?;
                RigModel::OpenAiChat(client.completion_model(config.model.clone()))
            }
            "openai-responses" => {
                let client = rig_core::providers::openai::Client::builder()
                    .api_key(api_key.clone())
                    .base_url(base_url)
                    .http_client(http_client)
                    .build()
                    .map_err(|_| {
                        "configuration_failed stage=configuration: invalid Responses client settings or API key header".to_string()
                    })?;
                RigModel::OpenAiResponses(client.completion_model(config.model.clone()))
            }
            "anthropic" => {
                let client = rig_core::providers::anthropic::Client::builder()
                    .api_key(api_key)
                    .base_url(base_url)
                    .http_client(http_client)
                    .build()
                    .map_err(|_| "configuration_failed stage=configuration: invalid Anthropic client settings or API key header".to_string())?;
                RigModel::Anthropic(client.completion_model(config.model.clone()))
            }
            _ => {
                return Err(format!(
                "unsupported LLM provider '{}'; use openai-chat, openai-responses, or anthropic",
                config.provider
            ))
            }
        };
        Ok(Self {
            model,
            provider,
            model_name,
            host,
            recorder: None,
            structured_output_disabled: AtomicBool::new(false),
        })
    }

    /// 记录一条请求事件（含服务商、模型、阶段、耗时）。
    fn event(
        &self,
        event: &str,
        stage: &str,
        retry: usize,
        elapsed_ms: u128,
    ) -> Result<(), String> {
        let details = serde_json::json!({"provider":self.provider,"model":self.model_name,"stage":stage,"endpoint_host":self.host,"retry_count":retry,"elapsed_ms":elapsed_ms});
        if let Some(recorder) = &self.recorder {
            recorder.record_event(event, details)?;
        }
        Ok(())
    }

    fn context(&self, event: &str, stage: &str, message: &str) -> String {
        format!(
            "{event} provider={} model={} stage={stage} endpoint_host={}: {message}",
            self.provider, self.model_name, self.host
        )
    }
}

#[async_trait]
impl TranslationClient for RigClient {
    async fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<CompletionOutput, String> {
        self.complete_attempt(system_prompt, user_prompt, 0, None)
            .await
    }

    async fn complete_attempt(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        retry: usize,
        schema: Option<Schema>,
    ) -> Result<CompletionOutput, String> {
        let stage = stage_from_prompt(system_prompt);
        let requested_schema =
            schema.is_some() && !self.structured_output_disabled.load(Ordering::Relaxed);
        let schema = if requested_schema { schema } else { None };
        match self
            .request(system_prompt, user_prompt, retry, schema)
            .await
        {
            Ok(output) => Ok(output),
            Err(RequestError::Provider(error))
                if requested_schema && is_unsupported_schema_response(&error) =>
            {
                // 服务端拒绝 json_schema response_format，本次运行后续请求降级为提示词级 JSON。
                self.structured_output_disabled
                    .store(true, Ordering::Relaxed);
                let _ = self.event("structured_output_unsupported", stage, retry, 0);
                match self.request(system_prompt, user_prompt, retry, None).await {
                    Ok(output) => Ok(output),
                    Err(RequestError::Provider(error)) => Err(self.provider_error(&error, stage)),
                    Err(RequestError::Rendered(message)) => Err(message),
                }
            }
            Err(RequestError::Provider(error)) => Err(self.provider_error(&error, stage)),
            Err(RequestError::Rendered(message)) => Err(message),
        }
    }
}

impl RigClient {
    /// 发起一次流式请求并组装最终文本；所有失败都会先记录事件再返回。
    async fn request(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        retry: usize,
        schema: Option<Schema>,
    ) -> Result<CompletionOutput, RequestError> {
        let started = std::time::Instant::now();
        let stage = stage_from_prompt(system_prompt);
        let response = match &self.model {
            RigModel::OpenAiChat(model) => {
                stream_completion(model.clone(), system_prompt, user_prompt, schema).await
            }
            RigModel::OpenAiResponses(model) => {
                stream_completion(model.clone(), system_prompt, user_prompt, schema).await
            }
            RigModel::Anthropic(model) => {
                stream_completion(model.clone(), system_prompt, user_prompt, schema).await
            }
        };
        let mut response = match response {
            Ok(response) => response,
            Err(error) => {
                let (event, _) = safe_completion_error(&error);
                self.event(event, stage, retry, started.elapsed().as_millis())
                    .map_err(RequestError::Rendered)?;
                return Err(RequestError::Provider(error));
            }
        };
        let mut stream_error = None;
        while let Some(item) = response.next().await {
            if let Err(error) = item {
                stream_error = Some(error);
                break;
            }
        }
        if let Some(error) = stream_error {
            let (event, _) = safe_completion_error(&error);
            self.event(event, stage, retry, started.elapsed().as_millis())
                .map_err(RequestError::Rendered)?;
            return Err(RequestError::Provider(error));
        }
        let response: CompletionResponse = response.into();
        if !response.usage.has_values() {
            self.event("usage_missing", stage, retry, started.elapsed().as_millis())
                .map_err(RequestError::Rendered)?;
        }
        let usage = response.usage;
        let has_non_text = response
            .choice
            .iter()
            .any(|content| !matches!(content, AssistantContent::Text(_)));
        let text = response
            .choice
            .into_iter()
            .filter_map(|content| match content {
                AssistantContent::Text(value) => Some(value.text),
                _ => None,
            })
            .collect::<String>();
        if text.trim().is_empty() {
            self.event(
                "response_parse_failed",
                stage,
                retry,
                started.elapsed().as_millis(),
            )
            .map_err(RequestError::Rendered)?;
            return Err(RequestError::Rendered(self.context(
                "response_parse_failed",
                stage,
                if has_non_text {
                    "non-text choice; use a text completion model"
                } else {
                    EMPTY_COMPLETION_ERROR
                },
            )));
        }
        self.event(
            "request_completed",
            stage,
            retry,
            started.elapsed().as_millis(),
        )
        .map_err(RequestError::Rendered)?;
        Ok(CompletionOutput { text, usage })
    }

    fn provider_error(&self, error: &CompletionError, stage: &str) -> String {
        let (event, message) = safe_completion_error(error);
        self.context(event, stage, message)
    }
}

/// 判断错误是否属于「服务端不支持 json_schema 结构化输出」。
fn is_unsupported_schema_response(error: &CompletionError) -> bool {
    let Some(status) = error
        .provider_response_status()
        .map(|status| status.as_u16())
    else {
        return false;
    };
    if !matches!(status, 400 | 422) {
        return false;
    }
    error
        .provider_response_body()
        .map(|body| {
            let body = body.to_ascii_lowercase();
            body.contains("response_format")
                || body.contains("json_schema")
                || body.contains("schema")
        })
        .unwrap_or(false)
}

/// 把底层错误压缩成「事件名 + 已脱敏文本」。
///
/// 服务商和传输层的错误字符串可能带有凭据或响应体，这里只返回固定文案。
fn safe_completion_error(error: &CompletionError) -> (&'static str, &'static str) {
    if error.to_string().contains(EMPTY_COMPLETION_ERROR) {
        return ("response_parse_failed", EMPTY_COMPLETION_ERROR);
    }
    match error
        .provider_response_status()
        .map(|status| status.as_u16())
    {
        Some(200..=299) => (
            "response_parse_failed",
            "invalid JSON or unsupported response format; check protocol and model",
        ),
        Some(401 | 403) => (
            "request_failed",
            "authentication failed; check API Key and permissions",
        ),
        Some(404) => (
            "request_failed",
            "model or endpoint not found; check model name and Base URL",
        ),
        Some(429) => (
            "request_failed",
            "rate limit exceeded; retry later or check quota",
        ),
        Some(500..=599) => ("request_failed", "provider unavailable; retry later"),
        Some(_) => (
            "request_failed",
            "provider rejected the request; check model and protocol settings",
        ),
        None => match error {
            CompletionError::JsonError(_)
            | CompletionError::ResponseError(_)
            | CompletionError::ProviderResponse(_)
            // 非 SSE、帧损坏或缺少终止记录都属于格式/协议问题，而非连接失败。
            | CompletionError::ProviderError(_) => (
                "response_parse_failed",
                "invalid or incomplete response; check protocol and model",
            ),
            _ => (
                "request_failed",
                "connection or request failed; check endpoint, network, timeout and model",
            ),
        },
    }
}
