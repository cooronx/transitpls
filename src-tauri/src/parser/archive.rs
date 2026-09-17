//! EPUB 压缩包读取与路径规范化。

use std::io::{self, Read};
use std::path::Path;
use zip::ZipArchive;

/// 读取压缩包内的文本条目。
pub(super) fn read_zip_entry<R: Read + io::Seek>(
    archive: &mut ZipArchive<R>,
    path: &str,
) -> Result<String, String> {
    let mut entry = archive
        .by_name(path)
        .map_err(|error| format!("EPUB entry '{path}' is missing: {error}"))?;
    let mut contents = String::new();
    entry
        .read_to_string(&mut contents)
        .map_err(|error| format!("failed to read EPUB entry '{path}': {error}"))?;
    Ok(contents)
}

/// 把 EPUB 内部相对路径规范化为以 `/` 分隔的压缩包路径，并折叠 `..`。
pub(crate) fn normalize_zip_path(base: &Path, relative: &str) -> String {
    let mut parts = Vec::new();
    for component in base.join(relative).components() {
        match component {
            std::path::Component::Normal(value) => parts.push(value.to_string_lossy().to_string()),
            std::path::Component::ParentDir => {
                let _ = parts.pop();
            }
            _ => {}
        }
    }
    parts.join("/")
}
