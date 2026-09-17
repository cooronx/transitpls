//! TXT 导出。

use super::punctuation::normalize_chinese_punctuation;
use super::{first_matching_heading, validate_snapshot};
use crate::state::ExportSnapshot;

/// 渲染整本书为纯文本：章节标题 + 段落，章节之间空两行。
pub fn render_txt(snapshot: &ExportSnapshot) -> Result<String, String> {
    validate_snapshot(snapshot)?;
    let mut rendered_chapters = Vec::with_capacity(snapshot.chapters.len());
    for chapter in &snapshot.chapters {
        let title = normalize_chinese_punctuation(
            chapter
                .target_title
                .as_deref()
                .expect("validated chapter title should exist"),
        );
        let mut paragraphs = vec![title.clone()];
        let skip_heading = first_matching_heading(chapter, &title);
        for (index, segment) in chapter.segments.iter().enumerate() {
            if Some(index) == skip_heading {
                continue;
            }
            let target = segment
                .target
                .as_deref()
                .expect("validated segment target should exist");
            paragraphs.push(normalize_chinese_punctuation(target));
        }
        rendered_chapters.push(paragraphs.join("\n\n"));
    }
    Ok(format!("{}\n", rendered_chapters.join("\n\n\n")))
}
