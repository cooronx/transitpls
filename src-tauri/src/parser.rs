use crate::model::{Chapter, Document, DocumentMetadata, ItemStatus, Segment, SegmentKind};
use quick_xml::escape::unescape;
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use zip::ZipArchive;

const DEFAULT_MAX_SEGMENT_CHARS: usize = 2_000;

type OpfData = (Option<String>, HashMap<String, String>, Vec<String>);

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
        "txt" => parse_txt(path, max_chars)?,
        "epub" => parse_epub(path, max_chars)?,
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

fn parse_txt(path: &Path, max_chars: usize) -> Result<(String, Vec<Chapter>, String), String> {
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

fn parse_epub(path: &Path, max_chars: usize) -> Result<(String, Vec<Chapter>, String), String> {
    let file = File::open(path).map_err(|error| format!("failed to open EPUB: {error}"))?;
    let mut archive =
        ZipArchive::new(file).map_err(|error| format!("invalid EPUB zip: {error}"))?;
    let container = read_zip_entry(&mut archive, "META-INF/container.xml")?;
    let opf_path = parse_rootfile_path(&container)?;
    let opf = read_zip_entry(&mut archive, &opf_path)?;
    let opf_dir = Path::new(&opf_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let (title, manifest, spine) = parse_opf(&opf)?;
    let mut chapters = Vec::new();
    for (ordinal, idref) in spine.into_iter().enumerate() {
        let Some(href) = manifest.get(&idref) else {
            continue;
        };
        let href = href.split('#').next().unwrap_or(href);
        let entry_path = normalize_zip_path(opf_dir, href);
        let html = read_zip_entry(&mut archive, &entry_path)?;
        let mut blocks = parse_xhtml_blocks(&html);
        if blocks.is_empty() {
            blocks = fallback_xhtml_blocks(&html);
        }
        let chapter_title = blocks
            .iter()
            .find(|(_, kind, text)| matches!(kind, SegmentKind::Heading) && !text.is_empty())
            .map(|(_, _, text)| text.clone())
            .unwrap_or_else(|| format!("Chapter {}", ordinal + 1));
        let content = blocks
            .into_iter()
            .filter(|(_, _, text)| !text.trim().is_empty())
            .map(|(_, kind, text)| (kind, text))
            .collect::<Vec<_>>();
        chapters.push(build_chapter_from_blocks(
            ordinal,
            chapter_title,
            content,
            max_chars,
        ));
    }
    if chapters.is_empty() {
        return Err("EPUB spine contains no readable chapters".to_string());
    }
    Ok((
        title.unwrap_or_else(|| "Untitled".to_string()),
        chapters,
        "epub".to_string(),
    ))
}

fn build_chapter(
    ordinal: usize,
    title: String,
    paragraphs: Vec<String>,
    max_chars: usize,
) -> Chapter {
    let blocks = paragraphs
        .into_iter()
        .map(|paragraph| (SegmentKind::Paragraph, paragraph))
        .collect();
    build_chapter_from_blocks(ordinal, title, blocks, max_chars)
}

fn build_chapter_from_blocks(
    ordinal: usize,
    title: String,
    blocks: Vec<(SegmentKind, String)>,
    max_chars: usize,
) -> Chapter {
    let chapter_id = format!("chapter-{}-{}", ordinal + 1, hash_text(&title));
    let mut seen = HashMap::<String, usize>::new();
    let mut segments = Vec::new();
    let mut segment_ordinal = 0;
    for (kind, source) in blocks {
        for chunk in split_long_text(&source, max_chars) {
            let normalized = normalize_source(&chunk);
            if normalized.is_empty() {
                continue;
            }
            let base = format!(
                "seg-{}",
                hash_text(&format!("{}{:?}{}", chapter_id, kind, normalized))
            );
            let count = seen.entry(base.clone()).or_insert(0);
            *count += 1;
            let id = if *count == 1 {
                base
            } else {
                format!("{}-{}", base, count)
            };
            segments.push(Segment {
                id,
                ordinal: segment_ordinal,
                source: chunk.clone(),
                target: None,
                kind: kind.clone(),
                status: ItemStatus::Pending,
                source_hash: hash_text(&chunk),
                meta: serde_json::json!({}),
            });
            segment_ordinal += 1;
        }
    }
    Chapter {
        id: chapter_id,
        title,
        status: ItemStatus::Pending,
        segments,
    }
}

fn split_txt_paragraphs(lines: &[String]) -> Vec<String> {
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

fn split_long_text(text: &str, max_chars: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return vec![text.trim().to_string()];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let hard_end = (start + max_chars).min(chars.len());
        let mut end = hard_end;
        if hard_end < chars.len() {
            for index in (start..hard_end).rev() {
                if is_sentence_boundary(chars[index]) {
                    end = index + 1;
                    break;
                }
            }
        }
        if end == start {
            end = hard_end;
        }
        let chunk: String = chars[start..end].iter().collect();
        if !chunk.trim().is_empty() {
            chunks.push(chunk.trim().to_string());
        }
        start = end;
    }
    chunks
}

fn is_sentence_boundary(value: char) -> bool {
    matches!(
        value,
        '.' | '!' | '?' | '\u{3002}' | '\u{ff01}' | '\u{ff1f}' | '\n'
    )
}

fn parse_xhtml_blocks(input: &str) -> Vec<(usize, SegmentKind, String)> {
    let mut reader = Reader::from_str(input);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut blocks = Vec::new();
    let mut current: Option<(usize, SegmentKind, String)> = None;
    let mut ordinal = 0;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let name = event.name().as_ref().to_ascii_lowercase();
                if let Some(kind) = block_kind(&name) {
                    current = Some((ordinal, kind, String::new()));
                    ordinal += 1;
                }
            }
            Ok(Event::Text(event)) => {
                if let Some((_, _, text)) = current.as_mut() {
                    let value = unescape(event.as_ref())
                        .map(|value| value.into_owned())
                        .unwrap_or_default();
                    if should_separate_text(text, &value) {
                        text.push(' ');
                    }
                    text.push_str(value.trim());
                }
            }
            Ok(Event::End(event)) => {
                let name = event.name().as_ref().to_ascii_lowercase();
                if block_kind(&name).is_some() {
                    if let Some(block) = current.take() {
                        blocks.push(block);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => return fallback_xhtml_blocks(input),
            _ => {}
        }
        buffer.clear();
    }
    blocks
}

fn should_separate_text(existing: &str, incoming: &str) -> bool {
    let Some(last) = existing.chars().last() else {
        return false;
    };
    let Some(first) = incoming
        .chars()
        .find(|character| !character.is_whitespace())
    else {
        return false;
    };
    !last.is_whitespace()
        && !matches!(
            first,
            '.' | ','
                | ';'
                | ':'
                | '!'
                | '?'
                | '。'
                | '，'
                | '；'
                | '：'
                | '！'
                | '？'
                | ')'
                | ']'
                | '}'
        )
        && !matches!(last, '(' | '[' | '{' | '“' | '「')
}

fn fallback_xhtml_blocks(input: &str) -> Vec<(usize, SegmentKind, String)> {
    let mut text = String::with_capacity(input.len());
    let mut in_tag = false;
    for character in input.chars() {
        match character {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(character),
            _ => {}
        }
    }
    split_txt_paragraphs(&text.lines().map(str::to_string).collect::<Vec<_>>())
        .into_iter()
        .enumerate()
        .map(|(ordinal, value)| (ordinal, SegmentKind::Paragraph, value))
        .collect()
}

fn block_kind(name: &str) -> Option<SegmentKind> {
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "title" => Some(SegmentKind::Heading),
        "blockquote" | "q" => Some(SegmentKind::Quote),
        "p" | "li" | "pre" | "div" => Some(SegmentKind::Paragraph),
        _ => None,
    }
}

fn local_name(value: &str) -> String {
    value
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn parse_rootfile_path(xml: &str) -> Result<String, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Empty(event)) | Ok(Event::Start(event))
                if local_name(event.name().as_ref()) == "rootfile" =>
            {
                for attribute in event.attributes().flatten() {
                    if attribute.key.as_ref() == "full-path" {
                        return attribute
                            .normalized_value(XmlVersion::Implicit1_0)
                            .map(|value| value.into_owned())
                            .map_err(|error| format!("invalid container path: {error}"));
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("invalid container.xml: {error}")),
            _ => {}
        }
        buffer.clear();
    }
    Err("container.xml does not declare a rootfile".to_string())
}

fn parse_opf(xml: &str) -> Result<OpfData, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut title = None;
    let mut manifest = HashMap::new();
    let mut spine = Vec::new();
    let mut current_element = String::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) | Ok(Event::Empty(event)) => {
                current_element = local_name(event.name().as_ref());
                if current_element == "item" {
                    let mut id = None;
                    let mut href = None;
                    let mut media_type = None;
                    for attribute in event.attributes().flatten() {
                        match attribute.key.as_ref() {
                            "id" => {
                                id = attribute
                                    .normalized_value(XmlVersion::Implicit1_0)
                                    .ok()
                                    .map(|v| v.into_owned())
                            }
                            "href" => {
                                href = attribute
                                    .normalized_value(XmlVersion::Implicit1_0)
                                    .ok()
                                    .map(|v| v.into_owned())
                            }
                            "media-type" => {
                                media_type = attribute
                                    .normalized_value(XmlVersion::Implicit1_0)
                                    .ok()
                                    .map(|v| v.into_owned())
                            }
                            _ => {}
                        }
                    }
                    if let (Some(id), Some(href), Some(media_type)) = (id, href, media_type) {
                        if media_type == "application/xhtml+xml" || media_type == "text/html" {
                            manifest.insert(id, href);
                        }
                    }
                } else if current_element == "itemref" {
                    for attribute in event.attributes().flatten() {
                        if attribute.key.as_ref() == "idref" {
                            if let Ok(value) = attribute.normalized_value(XmlVersion::Implicit1_0) {
                                spine.push(value.into_owned());
                            }
                        }
                    }
                }
            }
            Ok(Event::Text(event)) if current_element == "title" => {
                title = unescape(event.as_ref())
                    .ok()
                    .map(|value| value.into_owned());
            }
            Ok(Event::End(_)) => current_element.clear(),
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("invalid EPUB package metadata: {error}")),
            _ => {}
        }
        buffer.clear();
    }
    Ok((title, manifest, spine))
}

fn read_zip_entry<R: Read + io::Seek>(
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

fn normalize_zip_path(base: &Path, relative: &str) -> String {
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

fn decode_utf8(bytes: &[u8]) -> Result<String, String> {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    String::from_utf8(bytes.to_vec())
        .map_err(|_| "TXT input must be UTF-8 (BOM is supported)".to_string())
}

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

fn normalize_source(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn hash_text(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{parse_opf, parse_xhtml_blocks, split_long_text, SegmentKind};
    use std::path::Path;

    #[test]
    fn split_long_text_respects_unicode_character_limit() {
        let chunks = split_long_text("第一句。第二句。第三句。", 5);
        assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 5));
        assert_eq!(chunks.join(""), "第一句。第二句。第三句。");
    }

    #[test]
    fn parses_self_closing_opf_manifest_items_in_spine_order() {
        let xml = r#"
            <package xmlns:dc="http://purl.org/dc/elements/1.1/">
              <metadata><dc:title>Book</dc:title></metadata>
              <manifest>
                <item id="second" href="second.xhtml" media-type="application/xhtml+xml"/>
                <item id="first" href="first.xhtml" media-type="application/xhtml+xml"/>
              </manifest>
              <spine><itemref idref="first"/><itemref idref="second"/></spine>
            </package>
        "#;
        let (title, manifest, spine) = parse_opf(xml).expect("valid OPF");
        assert_eq!(title.as_deref(), Some("Book"));
        assert_eq!(
            manifest.get("first").map(String::as_str),
            Some("first.xhtml")
        );
        assert_eq!(spine, vec!["first", "second"]);
    }

    #[test]
    fn normalizes_relative_epub_paths() {
        assert_eq!(
            super::normalize_zip_path(Path::new("OEBPS"), "../Text/chapter.xhtml"),
            "Text/chapter.xhtml"
        );
    }

    #[test]
    fn preserves_inline_punctuation_when_collecting_xhtml_text() {
        let blocks = parse_xhtml_blocks("<p>Hello <em>world</em>.</p>");
        assert_eq!(blocks[0].1, SegmentKind::Paragraph);
        assert_eq!(blocks[0].2, "Hello world.");
    }

    #[test]
    fn accepts_prefixed_opf_namespaces() {
        let xml = r#"
            <opf:package xmlns:opf="urn:opf" xmlns:dc="http://purl.org/dc/elements/1.1/">
              <opf:metadata><dc:title>Book</dc:title></opf:metadata>
              <opf:manifest><opf:item id="one" href="one.xhtml" media-type="application/xhtml+xml"/></opf:manifest>
              <opf:spine><opf:itemref idref="one"/></opf:spine>
            </opf:package>
        "#;
        let (title, manifest, spine) = parse_opf(xml).expect("valid prefixed OPF");
        assert_eq!(title.as_deref(), Some("Book"));
        assert!(manifest.contains_key("one"));
        assert_eq!(spine, vec!["one"]);
    }
}
