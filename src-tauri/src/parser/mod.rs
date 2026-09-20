//! 源文件解析：把 TXT / EPUB 转换成统一的 `Document` 模型。
//!
//! TXT 按空行分段、按章节标题切分；EPUB 读取 container/OPF/导航文档，
//! 按 spine 顺序提取正文，并根据导航标题合并章节。

mod archive;
mod chapter;
mod epub;
mod opf;
mod text;
mod txt;
mod xhtml;

#[cfg(test)]
mod tests;

pub(crate) use archive::normalize_zip_path;
pub(crate) use opf::parse_chapter_navigation;
pub(crate) use text::{normalize_source, split_long_text};
pub(crate) use txt::txt_chapter_paragraphs;
pub(crate) use xhtml::{block_kind, local_name, parse_xhtml_blocks};

use crate::model::{Document, DocumentMetadata};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use zip::ZipArchive;

const DEFAULT_MAX_SEGMENT_CHARS: usize = 2_000;
const MAX_COVER_BYTES: u64 = 20 * 1024 * 1024;

/// 从 EPUB 中提取的封面图。
#[derive(Debug)]
pub struct EpubCover {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

/// 解析源文件为文档模型；`max_segment_chars` 为 0 时使用默认分段上限。
pub fn parse_document(
    path: &Path,
    source_language: Option<&str>,
    max_segment_chars: usize,
) -> Result<Document, String> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let max_chars = if max_segment_chars == 0 {
        DEFAULT_MAX_SEGMENT_CHARS
    } else {
        max_segment_chars
    };

    let (title, chapters, format) = match extension.as_str() {
        "txt" => txt::parse_txt(path, max_chars)?,
        "epub" => epub::parse_epub(path, max_chars)?,
        _ => return Err("unsupported input format; expected .txt or .epub".to_string()),
    };
    let language = source_language
        .map(str::to_string)
        .unwrap_or_else(|| "auto".to_string());
    Ok(Document {
        metadata: DocumentMetadata {
            title,
            source_language: language,
            target_language: "zh-CN".to_string(),
            source_format: format,
        },
        chapters,
    })
}

/// 读取 EPUB 封面图；非 EPUB 或无封面时返回 `None`。
pub fn extract_epub_cover(path: &Path) -> Result<Option<EpubCover>, String> {
    let is_epub = path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("epub"));
    if !is_epub {
        return Ok(None);
    }

    let file = File::open(path).map_err(|error| format!("failed to open EPUB: {error}"))?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| format!("invalid EPUB zip: {error}"))?;
    let container = archive::read_zip_entry(&mut archive, "META-INF/container.xml")?;
    let opf_path = opf::parse_rootfile_path(&container)?;
    let opf = archive::read_zip_entry(&mut archive, &opf_path)?;
    let Some((href, media_type)) = opf::parse_cover_reference(&opf)? else {
        return Ok(None);
    };
    let opf_dir = Path::new(&opf_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let cover_path = normalize_zip_path(opf_dir, href.split('#').next().unwrap_or(&href));
    let mut entry = archive
        .by_name(&cover_path)
        .map_err(|error| format!("failed to read EPUB cover {cover_path}: {error}"))?;
    if entry.size() > MAX_COVER_BYTES {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read EPUB cover {cover_path}: {error}"))?;
    if bytes.is_empty() {
        return Ok(None);
    }
    Ok(Some(EpubCover { media_type, bytes }))
}
