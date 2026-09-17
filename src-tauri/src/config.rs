//! 应用配置：默认值、TOML 读写与校验。
//!
//! 配置文件默认为工作目录下的 `transitpls.toml`，相对 `state_dir` 按配置文件所在目录解析。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const DEFAULT_CONFIG_FILE: &str = "transitpls.toml";

/// 应用配置根节点，未配置的字段使用默认值。
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct AppConfig {
    /// 语言设置。
    pub language: LanguageConfig,
    /// 模型与请求设置。
    pub llm: LlmConfig,
    /// 分段与批次预算。
    pub segment: SegmentConfig,
    /// 路径设置。
    pub paths: PathsConfig,
    /// 分析策略。
    pub analysis: AnalysisConfig,
    /// 翻译流水线选项。
    pub pipeline: PipelineConfig,
    /// 界面与并发等通用设置。
    pub general: GeneralConfig,
}

/// 通用设置。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct GeneralConfig {
    /// 工作台每页显示的段落数。
    pub visible_segments: usize,
    /// 重译并发数。
    pub retranslation_concurrency: usize,
    /// 润色批次并发数。
    pub polish_concurrency: usize,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            visible_segments: 100,
            retranslation_concurrency: 3,
            polish_concurrency: 3,
        }
    }
}

/// 翻译流水线选项。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PipelineConfig {
    /// 全书翻译完成后是否自动润色。
    pub polish: bool,
    /// 注入提示词的最近译文字符数上限。
    pub recent_context_chars: usize,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            polish: false,
            recent_context_chars: 2_000,
        }
    }
}

/// 语言设置。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LanguageConfig {
    /// 源语言代码，`auto` 表示自动识别。
    pub source: String,
    /// 目标语言代码，目前仅支持 `zh-CN`。
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

/// 模型与请求设置。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct LlmConfig {
    /// 服务商协议：`openai-chat`、`openai-compatible`、`openai-responses` 或 `anthropic`。
    pub provider: String,
    /// 接口基础地址，留空时按协议使用官方地址。
    pub base_url: Option<String>,
    /// 模型名称。
    pub model: String,
    /// 读取 API Key 的环境变量名；本地免密钥服务留空。
    pub api_key_env: String,
    /// 单次请求超时秒数。
    pub timeout_secs: u64,
    /// 每次调用的最大重试次数。
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
    /// 规范化配置：统一服务商名称与 Base URL，校验协议、模型与超时。
    pub fn normalized(&self) -> Result<Self, String> {
        let mut value = self.clone();
        value.provider = crate::credentials::normalize_provider(&self.provider);
        let fail = |message: &str| format!("configuration_failed stage=configuration: {message}");
        let suffix = match value.provider.as_str() {
            "openai-chat" | "openai-compatible" => "/chat/completions",
            "openai-responses" => "/responses",
            "anthropic" => "/messages",
            _ => return Err(fail("unsupported provider; use openai-chat, openai-compatible, openai-responses, or anthropic")),
        };
        let default_url = if value.provider == "anthropic" {
            "https://api.anthropic.com"
        } else {
            "https://api.openai.com/v1"
        };
        let mut url = url::Url::parse(self.base_url.as_deref().unwrap_or(default_url).trim())
            .map_err(|_| fail("Base URL must be a valid HTTP(S) URL"))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(fail("Base URL must be a valid HTTP(S) URL"));
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(fail("remove credentials, query parameters and fragments from Base URL; use API Key settings"));
        }
        let path = url.path().trim_end_matches('/');
        if (url.host_str() == Some("api.anthropic.com") && value.provider != "anthropic")
            || (url.host_str() == Some("api.openai.com") && value.provider == "anthropic")
        {
            return Err(fail(
                "provider protocol does not match the endpoint host; select the matching protocol",
            ));
        }
        for endpoint in [
            "/chat/completions",
            "/responses",
            "/messages",
            "/api/chat",
            "/api/generate",
        ] {
            if path.ends_with(endpoint) && endpoint != suffix {
                return Err(fail(&format!(
                    "provider={} expects {suffix}; correct the protocol or Base URL",
                    value.provider
                )));
            }
        }
        let mut base = path.strip_suffix(suffix).unwrap_or(path).to_string();
        if value.provider == "anthropic" {
            base = base.strip_suffix("/v1").unwrap_or(&base).to_string();
        }
        url.set_path(&base);
        value.base_url = Some(url.as_str().trim_end_matches('/').to_string());
        value.model = value.model.trim().to_string();
        value.api_key_env = value.api_key_env.trim().to_string();
        if value.model.is_empty() || value.timeout_secs == 0 {
            return Err(fail(
                "model must not be empty and timeout_secs must be greater than zero",
            ));
        }
        Ok(value)
    }

    /// 是否允许空 API Key：仅本地 OpenAI 兼容服务且未配置密钥环境变量时为 true。
    pub fn allows_empty_key(&self) -> bool {
        matches!(
            self.provider.trim().to_ascii_lowercase().as_str(),
            "openai-chat" | "openai-compatible"
        ) && self.api_key_env.trim().is_empty()
            && self
                .base_url
                .as_deref()
                .and_then(|value| url::Url::parse(value).ok())
                .is_some_and(|url| match url.host() {
                    Some(url::Host::Domain(host)) => host == "localhost",
                    Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                    Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                    None => false,
                })
    }

    /// 获取 API Key：优先桌面端保存的密钥，其次环境变量。
    pub fn api_key(&self) -> Result<String, String> {
        let config = self.normalized()?;
        if config.allows_empty_key() {
            return Ok(String::new());
        }
        let stored = crate::credentials::load_api_key(&config.provider)?;
        let environment = std::env::var(&config.api_key_env).ok();
        if let Some(value) = select_api_key(stored, environment) {
            return Ok(value);
        }
        Err(format!(
            "configuration_failed provider={} model={} stage=configuration: API key is not configured; open desktop Settings or configure the API Key environment variable",
            config.provider, config.model
        ))
    }
}

/// 桌面端密钥优先于环境变量。
fn select_api_key(stored: Option<String>, environment: Option<String>) -> Option<String> {
    stored.or_else(|| environment.filter(|value| !value.trim().is_empty()))
}

/// 分段与批次预算。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SegmentConfig {
    /// 单个段落的最大字符数，超过时按句末标点切分。
    pub max_chars_per_segment: usize,
    /// 单个翻译批次的最大字符数。
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

/// 路径设置。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PathsConfig {
    /// 项目数据目录，相对路径按配置文件所在目录解析。
    pub state_dir: PathBuf,
}

impl Default for PathsConfig {
    fn default() -> Self {
        Self {
            state_dir: PathBuf::from("projects"),
        }
    }
}

/// 分析策略。
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AnalysisConfig {
    /// 是否执行完整分析（逐章摘要与全书梗概）。
    pub full_book: bool,
}

impl Default for AnalysisConfig {
    fn default() -> Self {
        Self { full_book: true }
    }
}

/// 加载结果：配置内容、来源文件与解析后的状态目录。
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub value: AppConfig,
    pub path: Option<PathBuf>,
    pub state_dir: PathBuf,
}

/// 加载配置：显式路径必须存在，否则读取工作目录下的默认文件，都没有时使用默认值。
pub fn load(explicit_path: Option<&Path>) -> Result<LoadedConfig, String> {
    let cwd = std::env::current_dir()
        .map_err(|error| format!("failed to determine current directory: {error}"))?;
    let candidate = explicit_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cwd.join(DEFAULT_CONFIG_FILE));
    let (mut value, path): (AppConfig, _) = if candidate.exists() {
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
    value.llm = value.llm.normalized()?;
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

/// 校验配置中的取值范围。
fn validate(config: &AppConfig) -> Result<(), String> {
    config.llm.normalized()?;
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
    if config.pipeline.recent_context_chars == 0 {
        return Err("pipeline.recent_context_chars must be greater than zero".to_string());
    }
    if config.general.visible_segments == 0 {
        return Err("general.visible_segments must be greater than zero".to_string());
    }
    if config.general.retranslation_concurrency == 0 {
        return Err("general.retranslation_concurrency must be greater than zero".to_string());
    }
    if config.general.polish_concurrency == 0 {
        return Err("general.polish_concurrency must be greater than zero".to_string());
    }
    if config.llm.timeout_secs == 0 {
        return Err("llm.timeout_secs must be greater than zero".to_string());
    }
    Ok(())
}

/// 校验并保存配置到工作目录下的 `transitpls.toml`。
pub fn save_default(config: &AppConfig) -> Result<PathBuf, String> {
    validate(config)?;
    let mut config = config.clone();
    config.llm = config.llm.normalized()?;
    let path = std::env::current_dir()
        .map_err(|error| format!("failed to determine current directory: {error}"))?
        .join(DEFAULT_CONFIG_FILE);
    let text = toml::to_string_pretty(&config)
        .map_err(|error| format!("failed to serialize configuration: {error}"))?;
    std::fs::write(&path, text)
        .map_err(|error| format!("failed to write configuration: {error}"))?;
    Ok(path)
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
        assert!(!config.pipeline.polish);
        assert_eq!(config.pipeline.recent_context_chars, 2_000);
        assert_eq!(config.general.visible_segments, 100);
        assert_eq!(config.general.retranslation_concurrency, 3);
        assert_eq!(config.general.polish_concurrency, 3);
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
    fn prefers_desktop_api_key_over_environment_fallback() {
        assert_eq!(
            super::select_api_key(Some("desktop-key".into()), Some("environment-key".into())),
            Some("desktop-key".into())
        );
    }

    #[test]
    fn uses_environment_api_key_as_fallback() {
        assert_eq!(
            super::select_api_key(None, Some("environment-key".into())),
            Some("environment-key".into())
        );
    }
}
