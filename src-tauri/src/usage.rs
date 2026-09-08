use crate::state;
use chrono::{SecondsFormat, Utc};
use rig_core::completion::Usage;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub tool_use_prompt_tokens: u64,
    pub reasoning_tokens: u64,
}

impl TokenUsage {
    fn add(&mut self, other: &Self) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.total_tokens += other.total_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
        self.tool_use_prompt_tokens += other.tool_use_prompt_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
    }
}

impl From<Usage> for TokenUsage {
    fn from(value: Usage) -> Self {
        Self {
            input_tokens: value.input_tokens,
            output_tokens: value.output_tokens,
            total_tokens: value.total_tokens,
            cached_input_tokens: value.cached_input_tokens,
            cache_creation_input_tokens: value.cache_creation_input_tokens,
            tool_use_prompt_tokens: value.tool_use_prompt_tokens,
            reasoning_tokens: value.reasoning_tokens,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageEntry {
    pub stage: String,
    pub model: String,
    pub recorded_at: String,
    #[serde(flatten)]
    pub tokens: TokenUsage,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageFile {
    pub calls: Vec<UsageEntry>,
    pub totals: TokenUsage,
}

#[derive(Debug, Clone)]
pub struct UsageRecorder {
    path: PathBuf,
    model: String,
}

impl UsageRecorder {
    pub fn record_event(&self, event: &str, details: serde_json::Value) -> Result<(), String> {
        state::append_log_at(&self.path.with_file_name("logs.txt"), event, details)
    }
    pub fn record_failure(&self, stage: &str, details: serde_json::Value) -> Result<(), String> {
        state::append_log_at(
            &self.path.with_file_name("logs.txt"),
            "llm_failed",
            serde_json::json!({"stage": stage, "model": self.model, "details": details}),
        )
    }
    pub fn new(project_dir: &Path, model: impl Into<String>) -> Self {
        Self {
            path: project_dir.join("usage.json"),
            model: model.into(),
        }
    }

    pub fn record(&self, stage: &str, usage: Usage) -> Result<(), String> {
        let tokens = TokenUsage::from(usage);
        let mut file = if self.path.exists() {
            state::read_json(&self.path)?
        } else {
            UsageFile::default()
        };
        file.totals.add(&tokens);
        file.calls.push(UsageEntry {
            stage: stage.to_string(),
            model: self.model.clone(),
            recorded_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            tokens,
        });
        state::write_json_atomic(&self.path, &file)
    }
}

#[cfg(test)]
mod tests {
    use super::{UsageFile, UsageRecorder};
    use rig_core::completion::Usage;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn records_calls_and_accumulates_tokens_without_pricing() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("transitpls-usage-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp directory should be created");
        let recorder = UsageRecorder::new(&dir, "test-model");
        recorder
            .record(
                "translation",
                Usage {
                    input_tokens: 10,
                    output_tokens: 4,
                    total_tokens: 14,
                    ..Usage::default()
                },
            )
            .expect("first call should record");
        recorder
            .record(
                "polish",
                Usage {
                    input_tokens: 8,
                    output_tokens: 3,
                    total_tokens: 11,
                    ..Usage::default()
                },
            )
            .expect("second call should record");

        let value: UsageFile =
            crate::state::read_json(&dir.join("usage.json")).expect("usage file should load");
        assert_eq!(value.calls.len(), 2);
        assert_eq!(value.totals.input_tokens, 18);
        assert_eq!(value.totals.output_tokens, 7);
        assert_eq!(value.totals.total_tokens, 25);
        assert_eq!(value.calls[1].stage, "polish");
        assert_eq!(value.calls[1].model, "test-model");

        fs::remove_dir_all(dir).expect("temp directory should be removed");
    }
}
