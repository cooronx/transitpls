//! 成品导出：把完成的项目渲染为 TXT 或 EPUB 并原子写入磁盘。
//!
//! EPUB 有两种路径：源文件是 EPUB 时保留原始资源与排版，只回填译文（`epub::refill`）；
//! 源文件是 TXT 时生成一个基础 EPUB（`epub::generate`）。

mod atomic;
mod epub;
mod paragraphs;
mod punctuation;
mod txt;

#[cfg(test)]
mod tests;

use crate::model::ItemStatus;
use crate::state::ExportSnapshot;
use std::path::{Path, PathBuf};

pub use atomic::write_atomic;
pub use punctuation::normalize_chinese_punctuation;
pub use txt::render_txt;

#[derive(Debug, Default, Clone, Copy, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExportOptions {
    pub bilingual: bool,
    pub order: Option<ExportOrder>,
}

#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Deserialize,
    serde::Serialize,
    clap::ValueEnum,
)]
#[serde(rename_all = "kebab-case")]
pub enum ExportOrder {
    #[default]
    TargetFirst,
    SourceFirst,
}

impl ExportOptions {
    pub fn validate(self) -> Result<(), String> {
        if !self.bilingual && self.order.is_some() {
            return Err("export order requires bilingual mode".to_string());
        }
        Ok(())
    }
}

/// 导出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Txt,
    Epub,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Txt => "txt",
            Self::Epub => "epub",
        }
    }
}

/// 默认输出路径：源文件同级的 `output/{书名}.zh.{扩展名}`。
pub fn default_output_path(input: &Path, format: ExportFormat, options: ExportOptions) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("book");
    input
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("output")
        .join(format!(
            "{stem}.{}.{}",
            if options.bilingual { "zh-bi" } else { "zh" },
            format.extension()
        ))
}

/// 导出前校验：章节数量一致，且每章都有译文标题、每个段落都有非空译文。
pub fn validate_snapshot(snapshot: &ExportSnapshot) -> Result<(), String> {
    if snapshot.project.chapters_total != snapshot.chapters.len() {
        return Err("project snapshot chapter count is inconsistent".to_string());
    }
    for (chapter_index, chapter) in snapshot.chapters.iter().enumerate() {
        let title = chapter.target_title.as_deref().unwrap_or_default().trim();
        if title.is_empty() {
            return Err(format!(
                "chapter {chapter_index} ({}) has no translated title",
                chapter.id
            ));
        }
        for segment in &chapter.segments {
            if segment.status != ItemStatus::Translated {
                return Err(format!(
                    "chapter {chapter_index} segment {} is not translated",
                    segment.id
                ));
            }
            if segment
                .target
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err(format!(
                    "chapter {chapter_index} segment {} has an empty translation",
                    segment.id
                ));
            }
        }
    }
    Ok(())
}

/// 渲染 EPUB：按源格式选择回填或生成。
pub fn render_epub(snapshot: &ExportSnapshot, options: ExportOptions) -> Result<Vec<u8>, String> {
    options.validate()?;
    validate_snapshot(snapshot)?;
    if source_is_epub(snapshot) {
        if options.bilingual {
            return Err("bilingual EPUB refill is not yet supported".to_string());
        }
        epub::refill_epub(snapshot)
    } else {
        epub::generate_epub(snapshot, options)
    }
}

fn source_is_epub(snapshot: &ExportSnapshot) -> bool {
    Path::new(&snapshot.project.source_file)
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("epub"))
}
