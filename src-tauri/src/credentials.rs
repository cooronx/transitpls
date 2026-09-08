use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize, Serialize)]
struct StoredCredentials {
    keys: HashMap<String, String>,
}

pub fn load_api_key(provider: &str) -> Result<Option<String>, String> {
    load_api_key_at(&credentials_path()?, provider)
}

pub fn save_api_key(provider: &str, api_key: &str) -> Result<(), String> {
    save_api_key_at(&credentials_path()?, provider, api_key)
}

fn credentials_path() -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join("Library/Application Support"));
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")));

    base.map(|path| path.join("TransItPls/credentials.json"))
        .ok_or_else(|| "unable to determine the user configuration directory".to_string())
}

fn load_api_key_at(path: &Path, provider: &str) -> Result<Option<String>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(path)
        .map_err(|error| format!("failed to read desktop credentials: {error}"))?;
    let stored: StoredCredentials = serde_json::from_str(&text)
        .map_err(|error| format!("invalid desktop credentials file: {error}"))?;
    Ok(stored.keys.get(&normalize_provider(provider)).cloned())
}

fn save_api_key_at(path: &Path, provider: &str, api_key: &str) -> Result<(), String> {
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err("API Key 不能为空".to_string());
    }
    let mut stored = if path.exists() {
        let text = fs::read_to_string(path)
            .map_err(|error| format!("failed to read desktop credentials: {error}"))?;
        serde_json::from_str(&text)
            .map_err(|error| format!("invalid desktop credentials file: {error}"))?
    } else {
        StoredCredentials::default()
    };
    stored
        .keys
        .insert(normalize_provider(provider), api_key.to_string());
    let parent = path
        .parent()
        .ok_or_else(|| "credentials path has no parent".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create desktop config directory: {error}"))?;
    let bytes = serde_json::to_vec_pretty(&stored)
        .map_err(|error| format!("failed to serialize desktop credentials: {error}"))?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("failed to open desktop credentials: {error}"))?;
    file.write_all(&bytes)
        .map_err(|error| format!("failed to write desktop credentials: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("failed to flush desktop credentials: {error}"))?;
    #[cfg(unix)]
    fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .map_err(|error| format!("failed to protect desktop credentials: {error}"))?;
    Ok(())
}

fn normalize_provider(provider: &str) -> String {
    provider.trim().to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::{load_api_key_at, save_api_key_at};
    use std::fs;

    #[test]
    fn stores_keys_by_provider_without_exposing_other_values() {
        let root = std::env::temp_dir().join(format!(
            "transitpls-credentials-{}-{}",
            std::process::id(),
            std::thread::current()
                .name()
                .unwrap_or("test")
                .replace(':', "_")
        ));
        let path = root.join("credentials.json");
        save_api_key_at(&path, "OpenAI-Chat", "  sk-example  ").expect("key should be saved");
        assert_eq!(
            load_api_key_at(&path, "openai-chat").expect("key should load"),
            Some("sk-example".to_string())
        );
        assert_eq!(
            load_api_key_at(&path, "anthropic").expect("missing provider should load"),
            None
        );
        fs::remove_dir_all(root).expect("fixture should be removed");
    }
}
