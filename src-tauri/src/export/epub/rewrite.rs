//! EPUB XML 重写：回填译文，并同步更新目录（nav/NCX）与元数据。

use super::super::paragraphs::{translated_text, Paragraph};
use crate::model::Segment;
use crate::parser;
use quick_xml::escape::unescape;
use quick_xml::events::{BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer, XmlVersion};
use std::collections::HashMap;
use std::io::Cursor;
use std::path::Path;

/// 把一章保存的段落与 XHTML 块对齐，返回「块序号 -> 译文」的替换表。
///
/// 对齐以原文为基准：块按原文切分后的文本必须与保存的段落一一对应，
/// 不一致说明源文件与项目数据不匹配，直接报错而不是错误替换。
pub(super) fn align_chapter_document(
    chapter_index: usize,
    segments: &[&Segment],
    segment_index: &mut usize,
    xhtml: &str,
    max_chars: usize,
) -> Result<HashMap<usize, Paragraph>, String> {
    if max_chars == 0 {
        return Err("EPUB alignment failed: max_segment_chars must be positive".to_string());
    }
    let blocks = parser::parse_xhtml_blocks(xhtml);
    let mut replacements = HashMap::new();
    for (block_ordinal, kind, source) in blocks
        .into_iter()
        .filter(|(_, _, source)| !source.trim().is_empty())
    {
        let chunks = parser::split_long_text(&source, max_chars);
        let end = *segment_index + chunks.len();
        let Some(block_segments) = segments.get(*segment_index..end) else {
            return Err(format!(
                "EPUB alignment failed in chapter {chapter_index}: block {block_ordinal} has more source chunks than saved segments"
            ));
        };
        for (chunk, segment) in chunks.iter().zip(block_segments) {
            if segment.kind != kind
                || parser::normalize_source(chunk) != parser::normalize_source(&segment.source)
            {
                return Err(format!(
                    "EPUB alignment failed in chapter {chapter_index}, block {block_ordinal}, near {:?}",
                    source.chars().take(80).collect::<String>()
                ));
            }
        }
        replacements.insert(
            block_ordinal,
            Paragraph {
                source,
                target: translated_text(block_segments),
                kind,
            },
        );
        *segment_index = end;
    }
    Ok(replacements)
}

/// 在原始 XHTML 中替换块级元素的文本，保留标签与内联结构。
pub(in crate::export) fn rewrite_xhtml(
    xhtml: &str,
    replacements: &HashMap<usize, String>,
) -> Result<Vec<u8>, String> {
    let mut reader = Reader::from_str(xhtml);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buffer = Vec::new();
    let mut block_ordinal = 0;
    let mut element_stack = Vec::<Option<(String, bool)>>::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let is_block =
                    parser::block_kind(&parser::local_name(event.name().as_ref())).is_some();
                let replacement = is_block
                    .then(|| {
                        let ordinal = block_ordinal;
                        block_ordinal += 1;
                        replacements.get(&ordinal).cloned()
                    })
                    .flatten()
                    .map(|text| (text, false));
                writer
                    .write_event(Event::Start(event.into_owned()))
                    .map_err(|error| format!("failed to rewrite EPUB XHTML: {error}"))?;
                element_stack.push(replacement);
            }
            Ok(Event::Text(event)) => {
                // 命中替换的块只输出一次译文，空白的原有文本仍保留。
                if let Some((replacement, inserted)) =
                    element_stack.iter_mut().rev().find_map(Option::as_mut)
                {
                    let is_whitespace = unescape(event.as_ref())
                        .ok()
                        .is_some_and(|text| text.trim().is_empty());
                    if is_whitespace {
                        writer
                            .write_event(Event::Text(event.into_owned()))
                            .map_err(|error| format!("failed to rewrite EPUB XHTML: {error}"))?;
                    } else if !*inserted {
                        writer
                            .write_event(Event::Text(BytesText::new(replacement)))
                            .map_err(|error| format!("failed to rewrite EPUB XHTML: {error}"))?;
                        *inserted = true;
                    }
                } else {
                    writer
                        .write_event(Event::Text(event.into_owned()))
                        .map_err(|error| format!("failed to rewrite EPUB XHTML: {error}"))?;
                }
            }
            Ok(Event::CData(event)) => {
                if let Some((replacement, inserted)) =
                    element_stack.iter_mut().rev().find_map(Option::as_mut)
                {
                    let is_whitespace = event.as_ref().trim().is_empty();
                    if is_whitespace {
                        writer
                            .write_event(Event::CData(event.into_owned()))
                            .map_err(|error| format!("failed to rewrite EPUB XHTML: {error}"))?;
                    } else if !*inserted {
                        writer
                            .write_event(Event::Text(BytesText::new(replacement)))
                            .map_err(|error| format!("failed to rewrite EPUB XHTML: {error}"))?;
                        *inserted = true;
                    }
                } else {
                    writer
                        .write_event(Event::CData(event.into_owned()))
                        .map_err(|error| format!("failed to rewrite EPUB XHTML: {error}"))?;
                }
            }
            Ok(Event::GeneralRef(event)) => {
                if let Some((replacement, inserted)) =
                    element_stack.iter_mut().rev().find_map(Option::as_mut)
                {
                    if !*inserted {
                        writer
                            .write_event(Event::Text(BytesText::new(replacement)))
                            .map_err(|error| format!("failed to rewrite EPUB XHTML: {error}"))?;
                        *inserted = true;
                    }
                } else {
                    writer
                        .write_event(Event::GeneralRef(event.into_owned()))
                        .map_err(|error| format!("failed to rewrite EPUB XHTML: {error}"))?;
                }
            }
            Ok(Event::End(event)) => {
                // 空元素兜底：没有文本节点时在闭合前插入译文。
                if let Some((replacement, inserted)) =
                    element_stack.last_mut().and_then(Option::as_mut)
                {
                    if !*inserted {
                        writer
                            .write_event(Event::Text(BytesText::new(replacement)))
                            .map_err(|error| format!("failed to rewrite EPUB XHTML: {error}"))?;
                        *inserted = true;
                    }
                }
                let _ = element_stack.pop();
                writer
                    .write_event(Event::End(event.into_owned()))
                    .map_err(|error| format!("failed to rewrite EPUB XHTML: {error}"))?;
            }
            Ok(Event::Eof) => break,
            Ok(event) => writer
                .write_event(event.into_owned())
                .map_err(|error| format!("failed to rewrite EPUB XHTML: {error}"))?,
            Err(error) => return Err(format!("invalid EPUB XHTML while rewriting: {error}")),
        }
        buffer.clear();
    }
    Ok(writer.into_inner().into_inner())
}

/// 重写 EPUB3 nav 文档：按链接目标替换目录文字。
pub(super) fn rewrite_nav(
    nav: &str,
    nav_path: &str,
    titles: &HashMap<String, String>,
) -> Result<Vec<u8>, String> {
    let nav_dir = Path::new(nav_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let mut reader = Reader::from_str(nav);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buffer = Vec::new();
    let mut replacements = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let mut replacement = None;
                if parser::local_name(event.name().as_ref()) == "a" {
                    for attribute in event.attributes().flatten() {
                        if parser::local_name(attribute.key.as_ref()) == "href" {
                            if let Ok(href) = attribute.normalized_value(XmlVersion::Implicit1_0) {
                                let href = href.split('#').next().unwrap_or_default();
                                let target = parser::normalize_zip_path(nav_dir, href);
                                replacement = titles.get(&target);
                            }
                        }
                    }
                }
                writer
                    .write_event(Event::Start(event.into_owned()))
                    .map_err(|error| format!("failed to rewrite EPUB navigation: {error}"))?;
                if let Some(title) = replacement {
                    writer
                        .write_event(Event::Text(BytesText::new(title)))
                        .map_err(|error| format!("failed to rewrite EPUB navigation: {error}"))?;
                }
                replacements.push(replacement.is_some());
            }
            Ok(Event::Text(_)) if replacements.iter().any(|replaced| *replaced) => {}
            Ok(Event::CData(_)) if replacements.iter().any(|replaced| *replaced) => {}
            Ok(Event::End(event)) => {
                let _ = replacements.pop();
                writer
                    .write_event(Event::End(event.into_owned()))
                    .map_err(|error| format!("failed to rewrite EPUB navigation: {error}"))?;
            }
            Ok(Event::Eof) => break,
            Ok(event) => writer
                .write_event(event.into_owned())
                .map_err(|error| format!("failed to rewrite EPUB navigation: {error}"))?,
            Err(error) => return Err(format!("invalid EPUB navigation document: {error}")),
        }
        buffer.clear();
    }
    Ok(writer.into_inner().into_inner())
}

/// 重写 EPUB2 NCX 文档：按一级 navPoint 替换目录文字。
pub(super) fn rewrite_ncx(
    ncx: &str,
    ncx_path: &str,
    titles: &HashMap<String, String>,
) -> Result<Vec<u8>, String> {
    let ncx_dir = Path::new(ncx_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let targets = ncx_targets(ncx, ncx_dir, titles)?;
    let mut reader = Reader::from_str(ncx);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buffer = Vec::new();
    let mut nav_index = 0;
    let mut nav_stack = Vec::<Option<&String>>::new();
    let mut element_stack = Vec::<String>::new();
    let mut suppress_text = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let name = parser::local_name(event.name().as_ref());
                if name == "navpoint" {
                    nav_stack.push(targets.get(nav_index).and_then(Option::as_ref));
                    nav_index += 1;
                }
                writer
                    .write_event(Event::Start(event.into_owned()))
                    .map_err(|error| format!("failed to rewrite EPUB NCX: {error}"))?;
                if name == "text"
                    && element_stack
                        .last()
                        .is_some_and(|parent| parent == "navlabel")
                    && nav_stack.last().and_then(|title| *title).is_some()
                {
                    let title = nav_stack
                        .last()
                        .and_then(|title| *title)
                        .expect("checked title");
                    writer
                        .write_event(Event::Text(BytesText::new(title)))
                        .map_err(|error| format!("failed to rewrite EPUB NCX: {error}"))?;
                    suppress_text = true;
                }
                element_stack.push(name);
            }
            Ok(Event::Text(_)) | Ok(Event::CData(_)) if suppress_text => {}
            Ok(Event::End(event)) => {
                let name = parser::local_name(event.name().as_ref());
                if name == "text" {
                    suppress_text = false;
                }
                if name == "navpoint" {
                    let _ = nav_stack.pop();
                }
                let _ = element_stack.pop();
                writer
                    .write_event(Event::End(event.into_owned()))
                    .map_err(|error| format!("failed to rewrite EPUB NCX: {error}"))?;
            }
            Ok(Event::Eof) => break,
            Ok(event) => writer
                .write_event(event.into_owned())
                .map_err(|error| format!("failed to rewrite EPUB NCX: {error}"))?,
            Err(error) => return Err(format!("invalid EPUB NCX: {error}")),
        }
        buffer.clear();
    }
    Ok(writer.into_inner().into_inner())
}

/// 按 navPoint 顺序收集每个目录项对应的新标题。
fn ncx_targets(
    ncx: &str,
    ncx_dir: &Path,
    titles: &HashMap<String, String>,
) -> Result<Vec<Option<String>>, String> {
    let mut reader = Reader::from_str(ncx);
    let mut buffer = Vec::new();
    let mut targets = Vec::new();
    let mut nav_stack = Vec::<usize>::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let name = parser::local_name(event.name().as_ref());
                if name == "navpoint" {
                    nav_stack.push(targets.len());
                    targets.push(None);
                } else if name == "content" {
                    assign_ncx_target(&event, ncx_dir, titles, &nav_stack, &mut targets);
                }
            }
            Ok(Event::Empty(event)) if parser::local_name(event.name().as_ref()) == "content" => {
                assign_ncx_target(&event, ncx_dir, titles, &nav_stack, &mut targets);
            }
            Ok(Event::End(event)) if parser::local_name(event.name().as_ref()) == "navpoint" => {
                let _ = nav_stack.pop();
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("invalid EPUB NCX: {error}")),
            _ => {}
        }
        buffer.clear();
    }
    Ok(targets)
}

fn assign_ncx_target(
    event: &BytesStart<'_>,
    ncx_dir: &Path,
    titles: &HashMap<String, String>,
    nav_stack: &[usize],
    targets: &mut [Option<String>],
) {
    let Some(index) = nav_stack.last().copied() else {
        return;
    };
    for attribute in event.attributes().flatten() {
        if parser::local_name(attribute.key.as_ref()) == "src" {
            if let Ok(src) = attribute.normalized_value(XmlVersion::Implicit1_0) {
                let src = src.split('#').next().unwrap_or_default();
                let target = parser::normalize_zip_path(ncx_dir, src);
                targets[index] = titles.get(&target).cloned();
            }
        }
    }
}

/// 替换指定元素的文本内容（用于更新 OPF 中的语言标记）。
pub(super) fn replace_element_text(
    xml: &str,
    element: &str,
    replacement: &str,
) -> Result<Vec<u8>, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buffer = Vec::new();
    let mut replacing = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let matches = parser::local_name(event.name().as_ref()) == element;
                writer
                    .write_event(Event::Start(event.into_owned()))
                    .map_err(|error| format!("failed to rewrite EPUB metadata: {error}"))?;
                if matches {
                    writer
                        .write_event(Event::Text(BytesText::new(replacement)))
                        .map_err(|error| format!("failed to rewrite EPUB metadata: {error}"))?;
                    replacing = true;
                }
            }
            Ok(Event::Text(_)) | Ok(Event::CData(_)) if replacing => {}
            Ok(Event::End(event)) => {
                if parser::local_name(event.name().as_ref()) == element {
                    replacing = false;
                }
                writer
                    .write_event(Event::End(event.into_owned()))
                    .map_err(|error| format!("failed to rewrite EPUB metadata: {error}"))?;
            }
            Ok(Event::Eof) => break,
            Ok(event) => writer
                .write_event(event.into_owned())
                .map_err(|error| format!("failed to rewrite EPUB metadata: {error}"))?,
            Err(error) => return Err(format!("invalid EPUB metadata XML: {error}")),
        }
        buffer.clear();
    }
    Ok(writer.into_inner().into_inner())
}
