//! 导出段落：从原始 TXT 恢复续段归属，集中处理原译文组合规则。

use super::{normalize_chinese_punctuation, source_is_epub, ExportOptions, ExportOrder};
use crate::model::{Segment, SegmentKind};
use crate::{parser, state::ExportSnapshot};
use std::path::Path;

pub(super) const SOURCE_CSS: &str = r#"[data-transitpls-source] { display: block; font-size: 0.9em; color: #666; margin-block: 0.4em 1em; }
@media (prefers-color-scheme: dark) { [data-transitpls-source] { color: #aaa; } }"#;

pub(super) struct Paragraph {
    pub source: String,
    pub target: String,
    pub kind: SegmentKind,
}

impl Paragraph {
    pub fn texts(&self, options: ExportOptions) -> impl Iterator<Item = (bool, &str)> {
        let source = (true, self.source.as_str());
        let target = (false, self.target.as_str());
        let texts = match options.order.unwrap_or_default() {
            ExportOrder::TargetFirst => [target, source],
            ExportOrder::SourceFirst => [source, target],
        };
        let show_source = options.bilingual
            && self.kind != SegmentKind::Heading
            && !self.source.trim().is_empty()
            && self.source != self.target;
        texts
            .into_iter()
            .filter(move |(source, _)| !source || show_source)
    }
}

pub(super) fn translated_text(segments: &[&Segment]) -> String {
    normalize_chinese_punctuation(
        &segments
            .iter()
            .map(|segment| segment.target.as_deref().expect("validated translation"))
            .collect::<String>(),
    )
}

pub(super) fn chapters(
    snapshot: &ExportSnapshot,
    options: ExportOptions,
) -> Result<Vec<Vec<Paragraph>>, String> {
    let mut chapters = if !options.bilingual {
        snapshot
            .chapters
            .iter()
            .map(|chapter| {
                chapter
                    .segments
                    .iter()
                    .map(|segment| Paragraph {
                        source: segment.source.clone(),
                        target: translated_text(&[segment]),
                        kind: segment.kind.clone(),
                    })
                    .collect()
            })
            .collect()
    } else if source_is_epub(snapshot) {
        return Err("bilingual TXT from EPUB is not yet supported".to_string());
    } else {
        restore_txt(snapshot)?
    };
    for (paragraphs, chapter) in chapters.iter_mut().zip(&snapshot.chapters) {
        let title = normalize_chinese_punctuation(
            chapter.target_title.as_deref().expect("validated title"),
        );
        if let Some(index) = paragraphs
            .iter()
            .position(|p| p.kind == SegmentKind::Heading && p.target == title)
        {
            paragraphs.remove(index);
        }
    }
    Ok(chapters)
}

fn restore_txt(snapshot: &ExportSnapshot) -> Result<Vec<Vec<Paragraph>>, String> {
    let title = Path::new(&snapshot.project.source_file)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled");
    let originals = parser::txt_chapter_paragraphs(&snapshot.source_bytes, title)?;
    if originals.len() != snapshot.chapters.len() {
        return Err(
            "TXT alignment failed: source chapter count differs from saved chapters".to_string(),
        );
    }
    if snapshot.project.max_segment_chars == 0 {
        return Err("TXT alignment failed: max_segment_chars must be positive".to_string());
    }
    originals.into_iter().zip(&snapshot.chapters).enumerate().map(|(index, ((title, originals), chapter))| {
        if title != chapter.title {
            return Err(format!("TXT alignment failed in chapter {index}: source title differs"));
        }
        let mut segments = chapter.segments.iter().collect::<Vec<_>>();
        segments.sort_by_key(|s| s.ordinal);
        let mut remaining = segments.as_slice();
        let mut paragraphs = Vec::new();
        for source in originals {
            while remaining.first().is_some_and(|s| s.source.trim().is_empty()) {
                paragraphs.push(Paragraph { source: String::new(), target: translated_text(&remaining[..1]), kind: remaining[0].kind.clone() });
                remaining = &remaining[1..];
            }
            let chunks = parser::split_long_text(&source, snapshot.project.max_segment_chars);
            let count = chunks.len();
            let Some(matched) = remaining.get(..count).filter(|matched| chunks.iter().zip(*matched).all(|(chunk, s)| s.kind == SegmentKind::Paragraph && chunk == &s.source)) else {
                return Err(format!("TXT alignment failed in chapter {index}, paragraph {}: source chunks differ from saved segments", paragraphs.len()));
            };
            paragraphs.push(Paragraph { source, target: translated_text(matched), kind: SegmentKind::Paragraph });
            remaining = &remaining[count..];
        }
        for segment in remaining {
            if !segment.source.trim().is_empty() {
                return Err(format!("TXT alignment failed in chapter {index}: saved segments were not matched"));
            }
            paragraphs.push(Paragraph { source: String::new(), target: translated_text(&[segment]), kind: segment.kind.clone() });
        }
        Ok(paragraphs)
    }).collect()
}
