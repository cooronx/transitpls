use crate::model::{Chapter, ItemStatus, SegmentKind};
use crate::state::ExportSnapshot;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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

pub fn default_output_path(input: &Path, format: ExportFormat) -> PathBuf {
    let stem = input
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("book");
    input
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("output")
        .join(format!("{stem}.zh.{}", format.extension()))
}

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

fn first_matching_heading(chapter: &Chapter, normalized_title: &str) -> Option<usize> {
    chapter
        .segments
        .iter()
        .enumerate()
        .find(|(_, segment)| {
            segment.kind == SegmentKind::Heading
                && segment
                    .target
                    .as_deref()
                    .is_some_and(|target| normalize_chinese_punctuation(target) == normalized_title)
        })
        .map(|(index, _)| index)
}

pub fn normalize_chinese_punctuation(input: &str) -> String {
    let chars = input.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(input.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '`' {
            let end = chars[index + 1..]
                .iter()
                .position(|character| *character == '`')
                .map(|offset| index + offset + 2)
                .unwrap_or(chars.len());
            output.extend(chars[index..end].iter());
            index = end;
            continue;
        }
        if starts_url(&chars, index) {
            while index < chars.len() && !chars[index].is_whitespace() {
                output.push(chars[index]);
                index += 1;
            }
            continue;
        }
        if chars[index] == '.' {
            let start = index;
            while index < chars.len() && chars[index] == '.' {
                index += 1;
            }
            if index - start >= 3 {
                trim_trailing_whitespace(&mut output);
                output.push_str("……");
                continue;
            }
            for _ in start..index {
                output.push('.');
            }
            continue;
        }
        if chars[index] == '-' && chars.get(index + 1) == Some(&'-') {
            trim_trailing_whitespace(&mut output);
            output.push_str("——");
            index += 2;
            continue;
        }
        let replacement = match chars[index] {
            ',' => Some('，'),
            ';' => Some('；'),
            ':' => Some('：'),
            '!' => Some('！'),
            '?' => Some('？'),
            _ => None,
        };
        if let Some(replacement) = replacement {
            trim_trailing_whitespace(&mut output);
            output.push(replacement);
        } else {
            output.push(chars[index]);
        }
        index += 1;
    }
    output
}

fn starts_url(chars: &[char], index: usize) -> bool {
    let remaining = chars[index..].iter().collect::<String>();
    remaining.starts_with("http://")
        || remaining.starts_with("https://")
        || remaining.starts_with("www.")
}

fn trim_trailing_whitespace(value: &mut String) {
    while value.ends_with(char::is_whitespace) {
        value.pop();
    }
}

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
        replace_file(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn replace_file(temp: &Path, destination: &Path) -> Result<(), String> {
    #[cfg(windows)]
    if destination.exists() {
        fs::remove_file(destination)
            .map_err(|error| format!("failed to replace existing output file: {error}"))?;
    }
    fs::rename(temp, destination).map_err(|error| format!("failed to publish output file: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{normalize_chinese_punctuation, render_txt};
    use crate::model::{Chapter, ItemStatus, ProjectState, ProjectStatus, Segment, SegmentKind};
    use crate::state::ExportSnapshot;

    fn snapshot() -> ExportSnapshot {
        ExportSnapshot {
            project: ProjectState {
                id: "a".repeat(64),
                title: "Book".to_string(),
                source_file: "book.txt".to_string(),
                source_path: "/book.txt".to_string(),
                source_hash: "a".repeat(64),
                source_language: "en".to_string(),
                target_language: "zh-CN".to_string(),
                status: ProjectStatus::Translated,
                chapters_total: 1,
                chapters_completed: 1,
                created_at: String::new(),
                updated_at: String::new(),
                max_segment_chars: 1_200,
            },
            chapters: vec![Chapter {
                id: "chapter-1".to_string(),
                title: "Chapter 1".to_string(),
                target_title: Some("第一章".to_string()),
                status: ItemStatus::Translated,
                meta: serde_json::json!({}),
                segments: vec![
                    Segment {
                        id: "heading".to_string(),
                        ordinal: 0,
                        source: "Chapter 1".to_string(),
                        target: Some("第一章".to_string()),
                        target_before_polish: None,
                        kind: SegmentKind::Heading,
                        status: ItemStatus::Translated,
                        source_hash: String::new(),
                        meta: serde_json::json!({}),
                    },
                    Segment {
                        id: "body".to_string(),
                        ordinal: 1,
                        source: "Hello, world!".to_string(),
                        target: Some("你好, 世界!".to_string()),
                        target_before_polish: None,
                        kind: SegmentKind::Paragraph,
                        status: ItemStatus::Translated,
                        source_hash: String::new(),
                        meta: serde_json::json!({}),
                    },
                ],
            }],
            source_bytes: Vec::new(),
        }
    }

    #[test]
    fn punctuation_normalization_is_conservative() {
        assert_eq!(
            normalize_chinese_punctuation("你好 , world: https://a.b/x? ... -- `a:b?` ok"),
            "你好， world： https://a.b/x?……—— `a:b?` ok"
        );
    }

    #[test]
    fn txt_export_orders_content_and_avoids_duplicate_heading() {
        let rendered = render_txt(&snapshot()).expect("TXT should render");
        assert_eq!(rendered, "第一章\n\n你好， 世界！\n");
    }

    #[test]
    fn txt_export_rejects_incomplete_translation() {
        let mut snapshot = snapshot();
        snapshot.chapters[0].segments[1].target = None;
        let error = render_txt(&snapshot).expect_err("incomplete export should fail");
        assert!(error.contains("empty translation"));
    }
}
