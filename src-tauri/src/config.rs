use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const DEFAULT_CONFIG_FILE: &str = "transitpls.toml";

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct AppConfig {
    pub language: LanguageConfig,
    pub llm: LlmConfig,
    pub segment: SegmentConfig,
    pub paths: PathsConfig,
    pub analysis: AnalysisConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LanguageConfig {
    pub source: String,
    pub target: String,
}

impl Default for LanguageConfig {
    fn default() -> Self {
        Self {
            source: "auto".to_string(),
            target: "zh-CN".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub provider: String,
    pub base_url: Option<String>,
    pub model: String,
    pub api_key_env: String,
    pub timeout_secs: u64,
    pub max_retries: usize,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: "openai-chat".to_string(),
            base_url: Some("https://api.openai.com/v1".to_string()),
            model: "gpt-4o-mini".to_string(),
            api_key_env: "OPENAI_API_KEY".to_string(),
            timeout_secs: 60,
            max_retries: 3,
        }
    }
}

impl LlmConfig {
    pub fn api_key(&self) -> Result<String, String> {
        if self.api_key_env.trim().is_empty() {
            return Err("llm.api_key_env must not be empty".to_string());
        }
        std::env::var(&self.api_key_env).map_err(|_| {
            format!(
                "LLM API key environment variable '{}' is not set",
                self.api_key_env
            )
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SegmentConfig {
    pub max_chars_per_segment: usize,
    pub max_chars_per_batch: usize,
}

impl Default for SegmentConfig {
    fn default() -> Self {
        Self {
            max_chars_per_segment: 1_200,
            max_chars_per_batch: 1_800,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PathsConfig {
    pub state_dir: PathBuf,
}

impl Default for PathsConfig {
    fn default() -> Self {
        Self {
            state_dir: PathBuf::from("projects"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AnalysisConfig {
    pub full_book: bool,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self { full_book: true }
    }
}

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub value: AppConfig,
    pub path: Option<PathBuf>,
    pub state_dir: PathBuf,
}

pub fn load(explicit_path: Option<&Path>) -> Result<LoadedConfig, String> {
    let cwd = std::env::current_dir()
        .map_err(|error| format!("failed to determine current directory: {error}"))?;
    let candidate = explicit_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cwd.join(DEFAULT_CONFIG_FILE));
    let (value, path) = if candidate.exists() {
        let text = std::fs::read_to_string(&candidate)
            .map_err(|error| format!("failed to read config {}: {error}", candidate.display()))?;
        let parsed = toml::from_str(&text)
            .map_err(|error| format!("invalid TOML config {}: {error}", candidate.display()))?;
        (parsed, Some(candidate.clone()))
    } else if explicit_path.is_some() {
        return Err(format!(
            "specified config file does not exist: {}",
            candidate.display()
        ));
    } else {
        (AppConfig::default(), None)
    };
    validate(&value)?;
    let base = path.as_deref().and_then(Path::parent).unwrap_or(&cwd);
    let state_dir = if value.paths.state_dir.is_absolute() {
        value.paths.state_dir.clone()
    } else {
        base.join(&value.paths.state_dir)
    };
    Ok(LoadedConfig {
        value,
        path,
        state_dir,
    })
}

fn validate(config: &AppConfig) -> Result<(), String> {
    if config.language.source.trim().is_empty() {
        return Err("language.source must not be empty".to_string());
    }
    if config.language.target != "zh-CN" {
        return Err("language.target currently only supports 'zh-CN'".to_string());
    }
    if config.segment.max_chars_per_segment == 0 {
        return Err("segment.max_chars_per_segment must be greater than zero".to_string());
    }
    if config.segment.max_chars_per_batch == 0 {
        return Err("segment.max_chars_per_batch must be greater than zero".to_string());
    }
    if config.llm.timeout_secs == 0 {
        return Err("llm.timeout_secs must be greater than zero".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{load, AppConfig};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "transitpls-config-{name}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temp directory should be created");
        path
    }

    #[test]
    fn provides_documented_defaults() {
        let config = AppConfig::default();
        assert_eq!(config.language.source, "auto");
        assert_eq!(config.language.target, "zh-CN");
        assert_eq!(config.segment.max_chars_per_segment, 1_200);
        assert_eq!(config.segment.max_chars_per_batch, 1_800);
        assert!(config.analysis.full_book);
    }

    #[test]
    fn loads_explicit_config_and_resolves_relative_state_dir() {
        let dir = temp_dir("explicit");
        let path = dir.join("custom.toml");
        fs::write(
            &path,
            "[language]\nsource = \"ja\"\n[paths]\nstate_dir = \"state\"\n",
        )
        .expect("config should be written");

        let loaded = load(Some(&path)).expect("config should load");
        assert_eq!(loaded.value.language.source, "ja");
        assert_eq!(loaded.state_dir, dir.join("state"));
        fs::remove_dir_all(dir).expect("temp directory should be removed");
    }

    #[test]
    fn reads_api_key_from_named_environment_variable() {
        let mut config = AppConfig::default();
        let name = format!("TRANSITPLS_TEST_KEY_{}", std::process::id());
        config.llm.api_key_env = name.clone();
        std::env::set_var(&name, "secret-value");
        assert_eq!(
            config.llm.api_key().expect("key should resolve"),
            "secret-value"
        );
        std::env::remove_var(name);
    }
}
