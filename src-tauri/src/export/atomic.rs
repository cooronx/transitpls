//! 导出文件的原子写入。

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// 先写临时文件再重命名，避免导出中断留下半截文件。
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("output path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create output directory: {error}"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp = parent.join(format!(
        ".{}.tmp-{}-{stamp}",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("export"),
        std::process::id()
    ));
    let result = (|| {
        let mut file = File::create(&temp)
            .map_err(|error| format!("failed to create output temp file: {error}"))?;
        file.write_all(bytes)
            .map_err(|error| format!("failed to write output file: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("failed to sync output file: {error}"))?;
        fs::rename(&temp, path).map_err(|error| format!("failed to publish output file: {error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}
