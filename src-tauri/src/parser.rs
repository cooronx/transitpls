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
const MAX_COVER_BYTES: u64 = 20 * 1024 * 1024;

type OpfData = (
    Option<String>,
    HashMap<String, String>,
    Vec<String>,
    Option<String>,
    Option<String>,
);

#[derive(Debug)]
struct NavigationEntry {
    href: String,
    title: String,
}

#[derive(Debug)]
pub struct EpubCover {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
struct CoverManifestItem {
    href: String,
    media_type: String,
    is_epub3_cover: bool,
}

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
    let container = read_zip_entry(&mut archive, "META-INF/container.xml")?;
    let opf_path = parse_rootfile_path(&container)?;
    let opf = read_zip_entry(&mut archive, &opf_path)?;
    let Some((href, media_type)) = parse_cover_reference(&opf)? else {
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
    let (title, manifest, spine, navigation_href, ncx_href) = parse_opf(&opf)?;
    let navigation = if let Some(href) = navigation_href.or(ncx_href) {
        let path = normalize_zip_path(opf_dir, &href);
        let document = read_zip_entry(&mut archive, &path)?;
        let navigation_dir = Path::new(&path).parent().unwrap_or_else(|| Path::new(""));
        if href.ends_with(".ncx") {
            parse_ncx_navigation(&document)?
        } else {
            parse_navigation(&document)?
        }
        .into_iter()
        .map(|entry| {
            let href = entry.href.split('#').next().unwrap_or(&entry.href);
            (normalize_zip_path(navigation_dir, href), entry.title)
        })
        .filter(|(_, title)| !is_contents_title(title))
        .collect::<HashMap<_, _>>()
    } else {
        HashMap::new()
    };

    let mut documents = Vec::new();
    for idref in spine {
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
        let content = blocks
            .into_iter()
            .filter(|(_, _, text)| !text.trim().is_empty())
            .map(|(_, kind, text)| (kind, text))
            .collect::<Vec<_>>();
        documents.push((entry_path, content));
    }

    let chapters = if navigation.is_empty() {
        documents
            .into_iter()
            .enumerate()
            .map(|(ordinal, (_, content))| {
                let chapter_title = content
                    .iter()
                    .find(|(kind, text)| matches!(kind, SegmentKind::Heading) && !text.is_empty())
                    .map(|(_, text)| text.clone())
                    .unwrap_or_else(|| format!("Chapter {}", ordinal + 1));
                build_chapter_from_blocks(ordinal, chapter_title, content, max_chars)
            })
            .collect::<Vec<_>>()
    } else {
        group_spine_by_navigation(documents, &navigation, max_chars)
    };
    if chapters.is_empty() {
        return Err("EPUB spine contains no readable chapters".to_string());
    }
    Ok((
        title.unwrap_or_else(|| "Untitled".to_string()),
        chapters,
        "epub".to_string(),
    ))
}

fn group_spine_by_navigation(
    documents: Vec<(String, Vec<(SegmentKind, String)>)>,
    navigation: &HashMap<String, String>,
    max_chars: usize,
) -> Vec<Chapter> {
    let mut chapters = Vec::new();
    let mut current: Option<(String, Vec<(SegmentKind, String)>)> = None;
    let push_chapter =
        |chapters: &mut Vec<Chapter>, title: String, blocks: Vec<(SegmentKind, String)>| {
            let chapter = build_chapter_from_blocks(chapters.len(), title, blocks, max_chars);
            // Navigation files often contain structural entries without readable text.
            if !chapter.segments.is_empty() {
                chapters.push(chapter);
            }
        };
    for (path, content) in documents {
        if let Some(title) = navigation.get(&path) {
            if let Some((title, blocks)) = current.take() {
                push_chapter(&mut chapters, title, blocks);
            }
            current = Some((title.clone(), Vec::new()));
        }
        if let Some((_, blocks)) = current.as_mut() {
            blocks.extend(content);
        }
    }
    if let Some((title, blocks)) = current {
        push_chapter(&mut chapters, title, blocks);
    }
    chapters
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
                target_before_polish: None,
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
        target_title: None,
        status: ItemStatus::Pending,
        meta: serde_json::json!({}),
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

pub(crate) fn split_long_text(text: &str, max_chars: usize) -> Vec<String> {
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

pub(crate) fn parse_xhtml_blocks(input: &str) -> Vec<(usize, SegmentKind, String)> {
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

pub(crate) fn block_kind(name: &str) -> Option<SegmentKind> {
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Some(SegmentKind::Heading),
        "blockquote" | "q" => Some(SegmentKind::Quote),
        "p" | "li" | "pre" | "div" => Some(SegmentKind::Paragraph),
        _ => None,
    }
}

pub(crate) fn local_name(value: &str) -> String {
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
    let mut navigation_href = None;
    let mut ncx_href = None;
    let mut current_element = String::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) | Ok(Event::Empty(event)) => {
                current_element = local_name(event.name().as_ref());
                if current_element == "item" {
                    let mut id = None;
                    let mut href = None;
                    let mut media_type = None;
                    let mut properties = None;
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
                            "properties" => {
                                properties = attribute
                                    .normalized_value(XmlVersion::Implicit1_0)
                                    .ok()
                                    .map(|v| v.into_owned())
                            }
                            _ => {}
                        }
                    }
                    if let (Some(id), Some(href), Some(media_type)) = (id, href, media_type) {
                        if media_type == "application/x-dtbncx+xml" {
                            ncx_href = Some(href);
                        } else if media_type == "application/xhtml+xml" || media_type == "text/html"
                        {
                            if properties.as_deref().is_some_and(|value| {
                                value.split_whitespace().any(|item| item == "nav")
                            }) {
                                navigation_href = Some(href.clone());
                            }
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
    Ok((title, manifest, spine, navigation_href, ncx_href))
}

fn parse_cover_reference(xml: &str) -> Result<Option<(String, String)>, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut epub2_cover_id = None;
    let mut manifest = HashMap::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) | Ok(Event::Empty(event)) => {
                let name = local_name(event.name().as_ref());
                let attributes = event
                    .attributes()
                    .flatten()
                    .filter_map(|attribute| {
                        let key = local_name(attribute.key.as_ref());
                        attribute
                            .normalized_value(XmlVersion::Implicit1_0)
                            .ok()
                            .map(|value| (key, value.into_owned()))
                    })
                    .collect::<HashMap<_, _>>();
                if name == "meta" && attributes.get("name").is_some_and(|value| value == "cover") {
                    epub2_cover_id = attributes.get("content").cloned();
                } else if name == "item" {
                    let (Some(id), Some(href), Some(media_type)) = (
                        attributes.get("id"),
                        attributes.get("href"),
                        attributes.get("media-type"),
                    ) else {
                        buffer.clear();
                        continue;
                    };
                    if is_supported_cover_media_type(media_type) {
                        manifest.insert(
                            id.clone(),
                            CoverManifestItem {
                                href: href.clone(),
                                media_type: media_type.clone(),
                                is_epub3_cover: attributes.get("properties").is_some_and(|value| {
                                    value
                                        .split_whitespace()
                                        .any(|property| property == "cover-image")
                                }),
                            },
                        );
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("invalid EPUB package metadata: {error}")),
            _ => {}
        }
        buffer.clear();
    }

    let item = manifest
        .values()
        .find(|item| item.is_epub3_cover)
        .or_else(|| epub2_cover_id.as_ref().and_then(|id| manifest.get(id)));
    Ok(item.map(|item| (item.href.clone(), item.media_type.clone())))
}

fn is_supported_cover_media_type(value: &str) -> bool {
    matches!(
        value,
        "image/jpeg" | "image/png" | "image/gif" | "image/webp"
    )
}

fn parse_navigation(xml: &str) -> Result<Vec<NavigationEntry>, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut in_toc = false;
    let mut current_link: Option<NavigationEntry> = None;
    let mut entries = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let name = local_name(event.name().as_ref());
                if name == "nav" {
                    in_toc = event.attributes().flatten().any(|attribute| {
                        local_name(attribute.key.as_ref()) == "type"
                            && attribute
                                .normalized_value(XmlVersion::Implicit1_0)
                                .is_ok_and(|value| {
                                    value.split_whitespace().any(|item| item == "toc")
                                })
                    });
                } else if in_toc && name == "a" {
                    let href = event.attributes().flatten().find_map(|attribute| {
                        (local_name(attribute.key.as_ref()) == "href")
                            .then(|| {
                                attribute
                                    .normalized_value(XmlVersion::Implicit1_0)
                                    .ok()
                                    .map(|value| value.into_owned())
                            })
                            .flatten()
                    });
                    if let Some(href) = href {
                        current_link = Some(NavigationEntry {
                            href,
                            title: String::new(),
                        });
                    }
                }
            }
            Ok(Event::Text(event)) if current_link.is_some() => {
                if let Some(link) = current_link.as_mut() {
                    let value = unescape(event.as_ref())
                        .map(|value| value.into_owned())
                        .unwrap_or_default();
                    if should_separate_text(&link.title, &value) {
                        link.title.push(' ');
                    }
                    link.title.push_str(value.trim());
                }
            }
            Ok(Event::End(event)) => {
                let name = local_name(event.name().as_ref());
                if name == "a" {
                    if let Some(link) = current_link.take().filter(|link| !link.title.is_empty()) {
                        entries.push(link);
                    }
                } else if name == "nav" && in_toc {
                    in_toc = false;
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("invalid EPUB navigation document: {error}")),
            _ => {}
        }
        buffer.clear();
    }
    Ok(entries)
}

fn parse_ncx_navigation(xml: &str) -> Result<Vec<NavigationEntry>, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut navpoint_depth = 0usize;
    let mut in_nav_label = false;
    let mut current_href = None;
    let mut current_title = String::new();
    let mut entries = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let name = local_name(event.name().as_ref());
                if name == "navpoint" {
                    navpoint_depth += 1;
                    if navpoint_depth == 1 {
                        current_href = None;
                        current_title.clear();
                    }
                } else if name == "navlabel" && navpoint_depth == 1 {
                    in_nav_label = true;
                } else if name == "content" && navpoint_depth == 1 {
                    current_href = event.attributes().flatten().find_map(|attribute| {
                        (local_name(attribute.key.as_ref()) == "src")
                            .then(|| {
                                attribute
                                    .normalized_value(XmlVersion::Implicit1_0)
                                    .ok()
                                    .map(|v| v.into_owned())
                            })
                            .flatten()
                    });
                }
            }
            Ok(Event::Empty(event)) if local_name(event.name().as_ref()) == "content" => {
                if navpoint_depth == 1 {
                    current_href = event.attributes().flatten().find_map(|attribute| {
                        (local_name(attribute.key.as_ref()) == "src")
                            .then(|| {
                                attribute
                                    .normalized_value(XmlVersion::Implicit1_0)
                                    .ok()
                                    .map(|v| v.into_owned())
                            })
                            .flatten()
                    });
                }
            }
            Ok(Event::Text(event)) if in_nav_label => {
                let value = unescape(event.as_ref())
                    .map(|v| v.into_owned())
                    .unwrap_or_default();
                if !value.trim().is_empty() {
                    current_title.push_str(value.trim());
                }
            }
            Ok(Event::End(event)) => {
                let name = local_name(event.name().as_ref());
                if name == "navlabel" && navpoint_depth == 1 {
                    in_nav_label = false;
                } else if name == "navpoint" {
                    if navpoint_depth == 1 {
                        if let (Some(href), true) = (current_href.take(), !current_title.is_empty())
                        {
                            entries.push(NavigationEntry {
                                href,
                                title: std::mem::take(&mut current_title),
                            });
                        }
                        current_title.clear();
                    }
                    navpoint_depth = navpoint_depth.saturating_sub(1);
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("invalid EPUB NCX: {error}")),
            _ => {}
        }
        buffer.clear();
    }
    Ok(entries)
}

fn is_contents_title(title: &str) -> bool {
    matches!(title.trim(), "目次" | "Contents" | "Table of Contents")
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

pub(crate) fn normalize_source(value: &str) -> String {
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
    use super::{
        extract_epub_cover, parse_cover_reference, parse_document, parse_opf, parse_xhtml_blocks,
        split_long_text, SegmentKind,
    };
    use std::fs::{self, File};
    use std::io::Write;
    use std::path::Path;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

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
        let (title, manifest, spine, navigation, _) = parse_opf(xml).expect("valid OPF");
        assert_eq!(title.as_deref(), Some("Book"));
        assert_eq!(
            manifest.get("first").map(String::as_str),
            Some("first.xhtml")
        );
        assert_eq!(spine, vec!["first", "second"]);
        assert!(navigation.is_none());
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
        let (title, manifest, spine, navigation, _) = parse_opf(xml).expect("valid prefixed OPF");
        assert_eq!(title.as_deref(), Some("Book"));
        assert!(manifest.contains_key("one"));
        assert_eq!(spine, vec!["one"]);
        assert!(navigation.is_none());
    }

    #[test]
    fn recognizes_epub2_cover_metadata() {
        let xml = r#"
            <package>
              <metadata><meta name="cover" content="legacy-cover"/></metadata>
              <manifest>
                <item id="legacy-cover" href="images/front.png" media-type="image/png"/>
              </manifest>
            </package>
        "#;

        let cover = parse_cover_reference(xml)
            .expect("valid OPF")
            .expect("cover should be present");

        assert_eq!(
            cover,
            ("images/front.png".to_string(), "image/png".to_string())
        );
    }

    #[test]
    fn extracts_epub3_cover_image() {
        let root = std::env::temp_dir().join(format!(
            "transitpls-parser-cover-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::create_dir_all(&root).expect("fixture directory should be created");
        let path = root.join("book.epub");
        let file = File::create(&path).expect("fixture EPUB should be created");
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        for (name, contents) in [
            (
                "META-INF/container.xml",
                br#"<container><rootfiles><rootfile full-path="OPS/package.opf"/></rootfiles></container>"#.as_slice(),
            ),
            (
                "OPS/package.opf",
                br#"<package><manifest><item id="cover" href="images/cover.jpg" media-type="image/jpeg" properties="cover-image"/></manifest></package>"#.as_slice(),
            ),
            ("OPS/images/cover.jpg", &[0xff, 0xd8, 0xff, 0xd9]),
        ] {
            writer
                .start_file(name, options)
                .expect("fixture entry should start");
            writer
                .write_all(contents)
                .expect("fixture entry should be written");
        }
        writer.finish().expect("fixture EPUB should finish");

        let cover = extract_epub_cover(&path)
            .expect("EPUB should parse")
            .expect("cover should be extracted");

        assert_eq!(cover.media_type, "image/jpeg");
        assert_eq!(cover.bytes, [0xff, 0xd8, 0xff, 0xd9]);
        fs::remove_dir_all(root).expect("fixture should be removed");
    }

    #[test]
    fn uses_epub_navigation_to_group_spine_documents() {
        let root = std::env::temp_dir().join(format!(
            "transitpls-parser-nav-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        fs::create_dir_all(&root).expect("fixture directory should be created");
        let path = root.join("book.epub");
        let file = File::create(&path).expect("fixture EPUB should be created");
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        for (name, contents) in [
            (
                "META-INF/container.xml",
                r#"<container><rootfiles><rootfile full-path="OEBPS/package.opf"/></rootfiles></container>"#,
            ),
            (
                "OEBPS/package.opf",
                r#"<package xmlns:dc="http://purl.org/dc/elements/1.1/"><metadata><dc:title>Book</dc:title></metadata><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="first-start" href="first-start.xhtml" media-type="application/xhtml+xml"/><item id="first-body" href="first-body.xhtml" media-type="application/xhtml+xml"/><item id="second" href="second.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="first-start"/><itemref idref="first-body"/><itemref idref="second"/></spine></package>"#,
            ),
            (
                "OEBPS/nav.xhtml",
                r#"<html xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="first-start.xhtml#one">First chapter</a></li><li><a href="second.xhtml#two">Second chapter</a></li></ol></nav></body></html>"#,
            ),
            (
                "OEBPS/first-start.xhtml",
                r#"<html><head><title>Book</title></head><body><p id="one">First opening</p></body></html>"#,
            ),
            (
                "OEBPS/first-body.xhtml",
                r#"<html><head><title>Book</title></head><body><p>First continuation</p></body></html>"#,
            ),
            (
                "OEBPS/second.xhtml",
                r#"<html><head><title>Book</title></head><body><p id="two">Second body</p></body></html>"#,
            ),
        ] {
            writer
                .start_file(name, options)
                .expect("fixture entry should start");
            writer
                .write_all(contents.as_bytes())
                .expect("fixture entry should be written");
        }
        writer.finish().expect("fixture EPUB should finish");

        let document = parse_document(&path, Some("en"), 1_200).expect("EPUB should parse");

        assert_eq!(document.chapters.len(), 2);
        assert_eq!(document.chapters[0].title, "First chapter");
        assert_eq!(document.chapters[1].title, "Second chapter");
        assert!(document.chapters[0]
            .segments
            .iter()
            .any(|segment| segment.source == "First continuation"));
        assert!(document
            .chapters
            .iter()
            .flat_map(|chapter| &chapter.segments)
            .all(|segment| segment.source != "Book"));
        fs::remove_dir_all(root).expect("fixture should be removed");
    }

    #[test]
    fn uses_epub2_ncx_to_group_spine_documents() {
        let root =
            std::env::temp_dir().join(format!("transitpls-parser-ncx-{}", std::process::id()));
        fs::create_dir_all(&root).expect("fixture directory should be created");
        let path = root.join("book.epub");
        let file = File::create(&path).expect("fixture EPUB should be created");
        let mut writer = ZipWriter::new(file);
        let options = SimpleFileOptions::default();
        for (name, contents) in [
            (
                "META-INF/container.xml",
                r#"<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#,
            ),
            (
                "content.opf",
                r#"<package><metadata><dc:title xmlns:dc="http://purl.org/dc/elements/1.1/">Book</dc:title></metadata><manifest><item id="cover" href="cover.xhtml" media-type="application/xhtml+xml"/><item id="toc" href="toc.ncx" media-type="application/x-dtbncx+xml"/><item id="toc-page" href="toc-page.xhtml" media-type="application/xhtml+xml"/><item id="first" href="first.xhtml" media-type="application/xhtml+xml"/><item id="first-body" href="first-body.xhtml" media-type="application/xhtml+xml"/><item id="second" href="second.xhtml" media-type="application/xhtml+xml"/></manifest><spine toc="toc"><itemref idref="cover"/><itemref idref="toc-page"/><itemref idref="first"/><itemref idref="first-body"/><itemref idref="second"/></spine></package>"#,
            ),
            (
                "toc.ncx",
                r#"<ncx><navMap><navPoint><navLabel><text>目次</text></navLabel><content src="toc-page.xhtml"/></navPoint><navPoint><navLabel><text>First</text></navLabel><content src="first.xhtml"/></navPoint><navPoint><navLabel><text>Second</text></navLabel><content src="second.xhtml"/></navPoint></navMap></ncx>"#,
            ),
            ("cover.xhtml", "<html><body><p>Cover</p></body></html>"),
            (
                "toc-page.xhtml",
                "<html><body><p>Contents</p></body></html>",
            ),
            (
                "first.xhtml",
                "<html><body><p>First title</p></body></html>",
            ),
            (
                "first-body.xhtml",
                "<html><body><p>First body</p></body></html>",
            ),
            (
                "second.xhtml",
                "<html><body><p>Second body</p></body></html>",
            ),
        ] {
            writer
                .start_file(name, options)
                .expect("fixture entry should start");
            writer
                .write_all(contents.as_bytes())
                .expect("fixture entry should be written");
        }
        writer.finish().expect("fixture EPUB should finish");

        let document = parse_document(&path, Some("en"), 1_200).expect("EPUB should parse");

        assert_eq!(document.chapters.len(), 2);
        assert_eq!(document.chapters[0].title, "First");
        assert_eq!(document.chapters[1].title, "Second");
        assert!(document.chapters[0]
            .segments
            .iter()
            .any(|segment| segment.source == "First body"));
        fs::remove_dir_all(root).expect("fixture should be removed");
    }
}
