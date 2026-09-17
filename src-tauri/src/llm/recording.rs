//! 用量记录装饰器：把每次调用的 token 用量与失败事件写入 usage 记录。

use super::{stage_from_prompt, CompletionOutput, TranslationClient};
use async_trait::async_trait;
use schemars::Schema;

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
    fn record_failure(&self, stage: &str, details: serde_json::Value) -> Result<(), String> {
        self.recorder.record_failure(stage, details)
    }

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
        let output = match self
            .inner
            .complete_attempt(system_prompt, user_prompt, retry, schema)
            .await
        {
            Ok(output) => output,
            Err(error) => {
                self.record_failure(
                    stage_from_prompt(system_prompt),
                    serde_json::json!({"kind": "request", "error": error}),
                )
                .map_err(|log_error| {
                    format!("{error}; failed to record request error: {log_error}")
                })?;
                return Err(error);
            }
        };
        self.recorder
            .record(stage_from_prompt(system_prompt), output.usage)?;
        Ok(output)
    }
}
