//! JSON 原子读写、时间戳与 SHA-256 哈希工具。

use chrono::{SecondsFormat, Utc};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// 计算文件内容的 SHA-256，作为项目 ID 和源文件指纹。
pub fn hash_file(path: &Path) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|error| format!("failed to open input file: {error}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("failed to hash input file: {error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format_hash(hasher.finalize()))
}

/// 计算内存字节的 SHA-256，用于导出前校验源文件未变化。
pub(super) fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format_hash(hasher.finalize())
}

fn format_hash(hash: impl AsRef<[u8]>) -> String {
    hash.as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 读取 JSON 文件并反序列化。
pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&contents)
        .map_err(|error| format!("invalid JSON in {}: {error}", path.display()))
}

/// 以「临时文件 + 重命名」方式原子写入 JSON，避免中断留下半截文件。
pub(crate) fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "state path has no parent".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create state directory: {error}"))?;
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("failed to serialize state: {error}"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp = path.with_extension(format!("tmp-{}-{}", std::process::id(), stamp));
    {
        let mut file = File::create(&temp)
            .map_err(|error| format!("failed to create state temp file: {error}"))?;
        file.write_all(&bytes)
            .map_err(|error| format!("failed to write state: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("failed to sync state: {error}"))?;
    }
    if let Err(error) = fs::rename(&temp, path) {
        // Windows 不允许 rename 覆盖已存在文件，先删除再重命名。
        #[cfg(windows)]
        {
            if path.exists() {
                fs::remove_file(path).map_err(|remove_error| {
                    format!("failed to replace state file ({error}); remove failed: {remove_error}")
                })?;
                fs::rename(&temp, path).map_err(|rename_error| {
                    format!("failed to replace state file: {rename_error}")
                })?;
            } else {
                return Err(format!("failed to atomically replace state file: {error}"));
            }
        }
        #[cfg(not(windows))]
        {
            let _ = fs::remove_file(&temp);
            return Err(format!("failed to atomically replace state file: {error}"));
        }
    }
    Ok(())
}

/// 当前 UTC 时间，RFC 3339 毫秒精度。
pub(super) fn timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}
