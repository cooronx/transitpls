//! TXT 导出。

use super::punctuation::normalize_chinese_punctuation;
use super::{paragraphs, validate_snapshot, ExportOptions};
use crate::state::ExportSnapshot;

/// 渲染整本书为纯文本：章节标题 + 段落，章节之间空两行。
pub fn render_txt(snapshot: &ExportSnapshot, options: ExportOptions) -> Result<String, String> {
    options.validate()?;
    validate_snapshot(snapshot)?;
    let mut rendered_chapters = Vec::with_capacity(snapshot.chapters.len());
    for (chapter, blocks) in snapshot
        .chapters
        .iter()
        .zip(paragraphs::chapters(snapshot, options)?)
    {
        let title = normalize_chinese_punctuation(
            chapter
                .target_title
                .as_deref()
                .expect("validated chapter title should exist"),
        );
        let mut paragraphs = vec![title.clone()];
        for block in blocks {
            paragraphs.push(
                block
                    .texts(options)
                    .map(|(_, text)| text)
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        rendered_chapters.push(paragraphs.join("\n\n"));
    }
    Ok(format!("{}\n", rendered_chapters.join("\n\n\n")))
}
