//! TXT 解析：按章节标题切分，再按空行分段。

use super::chapter::build_chapter;
use super::text::decode_utf8;
use crate::model::Chapter;
use std::path::Path;

pub(super) fn parse_txt(
    path: &Path,
    max_chars: usize,
) -> Result<(String, Vec<Chapter>, String), String> {
    let bytes = std::fs::read(path).map_err(|error| format!("failed to read TXT: {error}"))?;
    let text = decode_utf8(&bytes)?;
    let title = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("Untitled")
        .to_string();
    let lines: Vec<&str> = text.lines().collect();
    let mut chapter_ranges: Vec<(String, Vec<String>)> = Vec::new();
    let mut current_title = title.clone();
    let mut current_lines = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if is_chapter_heading(trimmed) && !current_lines.is_empty() {
            chapter_ranges.push((current_title, current_lines));
            current_title = trimmed.to_string();
            current_lines = Vec::new();
        } else if is_chapter_heading(trimmed) && current_lines.is_empty() {
            current_title = trimmed.to_string();
        } else {
            current_lines.push(line.to_string());
        }
    }
    if !current_lines.is_empty() || chapter_ranges.is_empty() {
        chapter_ranges.push((current_title, current_lines));
    }

    let chapters = chapter_ranges
        .into_iter()
        .enumerate()
        .map(|(ordinal, (chapter_title, lines))| {
            let paragraphs = split_txt_paragraphs(&lines);
            build_chapter(ordinal, chapter_title, paragraphs, max_chars)
        })
        .collect();
    Ok((title, chapters, "txt".to_string()))
}

/// 连续非空行合并为一段，空行分段。
pub(super) fn split_txt_paragraphs(lines: &[String]) -> Vec<String> {
    let mut paragraphs = Vec::new();
    let mut current = String::new();
    for line in lines {
        if line.trim().is_empty() {
            if !current.trim().is_empty() {
                paragraphs.push(current.trim().to_string());
                current.clear();
            }
        } else {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(line.trim());
        }
    }
    if !current.trim().is_empty() {
        paragraphs.push(current.trim().to_string());
    }
    paragraphs
}

/// 识别章节标题：中文「第X章/节」、英文 `Chapter ` 前缀，或整行短大写标题。
fn is_chapter_heading(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    (value.starts_with('第') && (value.contains('章') || value.contains('节')))
        || lower.starts_with("chapter ")
        || (value.chars().count() <= 80
            && value.chars().any(char::is_uppercase)
            && value
                .chars()
                .filter(|character| character.is_alphabetic())
                .all(char::is_uppercase)
            && !value.contains(['.', ',', ';', '。', '，', '；']))
}
