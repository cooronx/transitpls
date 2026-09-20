//! 双语回填：只复制正文内联结构，先分配全书锚点，再重写跨文档链接。

use super::{package::read_entry_string, AlignedDocument};
use crate::export::{paragraphs::SOURCE_CSS, ExportOptions, ExportOrder};
use crate::{parser, state::ExportSnapshot};
use quick_xml::events::{BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer, XmlVersion};
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use zip::ZipArchive;

struct Document {
    events: Vec<Event<'static>>,
    ends: HashMap<usize, usize>,
    blocks: HashMap<usize, usize>,
    parents: HashMap<usize, usize>,
}

impl Document {
    fn parse(xml: &str) -> Result<Self, String> {
        let mut reader = Reader::from_str(xml);
        let mut document = Self {
            events: Vec::new(),
            ends: HashMap::new(),
            blocks: HashMap::new(),
            parents: HashMap::new(),
        };
        let mut stack = Vec::new();
        let mut ordinal = 0;
        loop {
            let event = reader
                .read_event()
                .map_err(|e| format!("invalid EPUB XHTML: {e}"))?
                .into_owned();
            let index = document.events.len();
            match &event {
                Event::Start(start) => {
                    if let Some(parent) = stack.last() {
                        document.parents.insert(index, *parent);
                    }
                    if parser::block_kind(&parser::local_name(start.name().as_ref())).is_some() {
                        document.blocks.insert(ordinal, index);
                        ordinal += 1;
                    }
                    stack.push(index);
                }
                Event::Empty(_) => {
                    if let Some(parent) = stack.last() {
                        document.parents.insert(index, *parent);
                    }
                    document.ends.insert(index, index);
                }
                Event::End(_) => {
                    let start = stack
                        .pop()
                        .ok_or("invalid EPUB XHTML: unmatched closing tag")?;
                    document.ends.insert(start, index);
                }
                Event::Eof => break,
                _ => {}
            }
            document.events.push(event);
        }
        if !stack.is_empty() {
            return Err("invalid EPUB XHTML: unclosed element".to_string());
        }
        Ok(document)
    }

    fn start(&self, index: usize) -> Option<&BytesStart<'static>> {
        match &self.events[index] {
            Event::Start(e) | Event::Empty(e) => Some(e),
            _ => None,
        }
    }

    fn anchors(&self) -> Result<HashSet<String>, String> {
        let mut result = HashSet::new();
        for event in &self.events {
            if let Event::Start(e) | Event::Empty(e) = event {
                let mut local = HashSet::new();
                for (key, value) in attributes(e)? {
                    if is_anchor(&key)
                        && local.insert(value.clone())
                        && !result.insert(value.clone())
                    {
                        return Err(format!("EPUB has duplicate anchor {value:?}"));
                    }
                }
            }
        }
        Ok(result)
    }

    fn validate_blocks(&self, aligned: &AlignedDocument) -> Result<(), String> {
        let mut ranges = aligned
            .blocks
            .keys()
            .map(|ordinal| {
                let start = self.blocks[ordinal];
                (start, self.ends[&start])
            })
            .collect::<Vec<_>>();
        ranges.sort_unstable();
        if ranges.windows(2).any(|pair| pair[1].0 < pair[0].1) {
            return Err(format!(
                "EPUB alignment failed: overlapping text blocks in {}",
                aligned.path
            ));
        }
        let mut ranges = ranges.into_iter().peekable();
        let mut index = 0;
        let mut in_body = false;
        while index < self.events.len() {
            if ranges.peek().is_some_and(|(start, _)| *start == index) {
                index = ranges.next().expect("checked range").1 + 1;
                continue;
            }
            match &self.events[index] {
                Event::Start(e) | Event::Empty(e) => {
                    let name = parser::local_name(e.name().as_ref());
                    if name == "body" {
                        in_body = true;
                    }
                    if is_resource(&name) {
                        index = self.ends[&index] + 1;
                        continue;
                    }
                }
                Event::End(e) if parser::local_name(e.name().as_ref()) == "body" => in_body = false,
                Event::Text(e) if in_body && !e.as_ref().trim().is_empty() => {
                    return Err(format!(
                        "EPUB alignment failed in {}: mixed nested text was not saved as a segment",
                        aligned.path
                    ));
                }
                Event::CData(_) | Event::GeneralRef(_) if in_body => {
                    return Err(format!(
                        "EPUB alignment failed in {}: unsaved text outside paragraph blocks",
                        aligned.path
                    ));
                }
                _ => {}
            }
            index += 1;
        }
        Ok(())
    }
}

fn attributes(event: &BytesStart<'_>) -> Result<Vec<(String, String)>, String> {
    event
        .attributes()
        .map(|attribute| {
            let attribute = attribute.map_err(|e| format!("invalid EPUB attribute: {e}"))?;
            let value = attribute
                .normalized_value(XmlVersion::Implicit1_0)
                .map_err(|e| format!("invalid EPUB attribute: {e}"))?;
            Ok((attribute.key.as_ref().to_string(), value.into_owned()))
        })
        .collect()
}

fn is_anchor(key: &str) -> bool {
    matches!(key, "id" | "xml:id" | "name")
}

fn is_resource(name: &str) -> bool {
    matches!(
        name,
        "img"
            | "svg"
            | "picture"
            | "audio"
            | "video"
            | "object"
            | "iframe"
            | "canvas"
            | "script"
            | "style"
    )
}

fn fresh_id(anchors: &mut HashSet<String>, next: &mut usize) -> String {
    loop {
        *next += 1;
        let id = format!("transitpls-source-{next}");
        if anchors.insert(id.clone()) {
            return id;
        }
    }
}

type AnchorMap = HashMap<String, HashMap<String, String>>;

pub(super) fn validate(aligned: &AlignedDocument) -> Result<(), String> {
    Document::parse(&aligned.xhtml)?.validate_blocks(aligned)
}

pub(super) fn rewrite_book(
    chapters: &[Vec<AlignedDocument>],
    archive: &mut ZipArchive<Cursor<&[u8]>>,
    snapshot: &ExportSnapshot,
    options: ExportOptions,
    navigation_paths: &HashSet<String>,
) -> Result<HashMap<String, Vec<u8>>, String> {
    let mut anchors = HashMap::new();
    let resources = archive
        .file_names()
        .map(str::to_string)
        .collect::<HashSet<_>>();
    // 非 spine 的脚注页、目录和 SVG 也可能是链接目标。
    for path in &resources {
        if matches!(
            path.rsplit('.').next(),
            Some("xhtml" | "html" | "htm" | "svg" | "xml")
        ) {
            let xml = read_entry_string(archive, path)?;
            anchors.insert(path.clone(), Document::parse(&xml)?.anchors()?);
        }
    }
    let mut documents = Vec::new();
    let mut mapping = AnchorMap::new();
    // 目录始终只有译文；后续 nav 重写不会保留任何原文副本。
    for aligned in chapters
        .iter()
        .flatten()
        .filter(|document| !navigation_paths.contains(&document.path))
    {
        let document = Document::parse(&aligned.xhtml)?;
        document.validate_blocks(aligned)?;
        let reserved = anchors
            .entry(aligned.path.clone())
            .or_insert(document.anchors()?);
        let mut next = 0;
        let mut copies = HashMap::new();
        let mut local = HashMap::new();
        let mut ordinals = aligned.blocks.keys().copied().collect::<Vec<_>>();
        ordinals.sort_unstable();
        for ordinal in ordinals {
            let block = &aligned.blocks[&ordinal];
            let start = *document
                .blocks
                .get(&ordinal)
                .ok_or("EPUB alignment has no matching XML element")?;
            let end = document.ends[&start];
            if block.texts(options).count() < 2 {
                continue;
            }
            let wrapper = fresh_id(reserved, &mut next);
            copies.insert(start, wrapper.clone());
            // 容器（如 aside）的锚点指向其第一个原文块；图片仍指向唯一的原资源。
            let mut ancestor = Some(start);
            while let Some(index) = ancestor {
                if let Some(element) = document.start(index) {
                    for (key, value) in attributes(element)? {
                        if is_anchor(&key) {
                            local.entry(value).or_insert_with(|| wrapper.clone());
                        }
                    }
                }
                ancestor = document.parents.get(&index).copied();
            }
            let mut index = start + 1;
            while index < end {
                if let Some(element) = document.start(index) {
                    if is_resource(&parser::local_name(element.name().as_ref())) {
                        index = document.ends[&index] + 1;
                        continue;
                    }
                    for (key, value) in attributes(element)? {
                        if is_anchor(&key) {
                            local
                                .entry(value)
                                .or_insert_with(|| fresh_id(reserved, &mut next));
                        }
                    }
                }
                index += 1;
            }
        }
        mapping.insert(aligned.path.clone(), local);
        documents.push((aligned, document, copies));
    }
    let mut output = HashMap::new();
    for (aligned, document, copies) in documents {
        let mut edits = HashMap::new();
        for (ordinal, paragraph) in &aligned.blocks {
            if paragraph.source == paragraph.target {
                continue;
            }
            let start = document.blocks[ordinal];
            let end = document.ends[&start];
            let outer = document.start(start).expect("block start");
            let span = qualified_name(outer, "span");
            let mut target = Writer::new(Vec::new());
            let mut target_start = BytesStart::new(&span);
            target_start.push_attribute(("lang", snapshot.project.target_language.as_str()));
            write(&mut target, Event::Start(target_start.clone()))?;
            write(&mut target, Event::Text(BytesText::new(&paragraph.target)))?;
            // 译文没有可靠的字符位置映射；脚注引用集中放在译文末尾，原文引用留在原位。
            write_target_extras(&document, start + 1, end, &mut target)?;
            write(&mut target, Event::End(target_start.to_end()))?;
            let target = target.into_inner();
            let mut contents = Writer::new(Vec::new());
            if let Some(id) = copies.get(&start) {
                let mut source_start = BytesStart::new(&span);
                source_start.push_attribute(("id", id.as_str()));
                source_start.push_attribute(("data-transitpls-source", ""));
                source_start.push_attribute(("lang", snapshot.project.source_language.as_str()));
                let mut source = Writer::new(Vec::new());
                write(&mut source, Event::Start(source_start.clone()))?;
                write_source(
                    &document,
                    start + 1,
                    end,
                    &aligned.path,
                    &mapping,
                    &anchors,
                    &resources,
                    &mut source,
                )?;
                write(&mut source, Event::End(source_start.to_end()))?;
                let source = source.into_inner();
                for bytes in if options.order.unwrap_or_default() == ExportOrder::SourceFirst {
                    [&source, &target]
                } else {
                    [&target, &source]
                } {
                    contents.get_mut().extend_from_slice(bytes);
                }
            } else {
                contents.get_mut().extend_from_slice(&target);
            }
            edits.insert(start, (end, contents.into_inner()));
        }
        let mut writer = Writer::new(Vec::new());
        let mut index = 0;
        let mut styled = false;
        while index < document.events.len() {
            if let Some((end, contents)) = edits.get(&index) {
                write(&mut writer, document.events[index].clone())?;
                writer.get_mut().extend_from_slice(contents);
                write(&mut writer, document.events[*end].clone())?;
                index = end + 1;
                continue;
            }
            // 放在 head 中，避免 CSS 被阅读器当成正文；无 head 的旧书补一个。
            if let Event::End(end) = &document.events[index] {
                if parser::local_name(end.name().as_ref()) == "head" {
                    let name = end.name().as_ref().replace("head", "style");
                    let style = BytesStart::new(name);
                    write(&mut writer, Event::Start(style.clone()))?;
                    write(&mut writer, Event::Text(BytesText::new(SOURCE_CSS)))?;
                    write(&mut writer, Event::End(style.to_end()))?;
                    styled = true;
                }
            }
            if !styled {
                if let Some(body) = document
                    .start(index)
                    .filter(|e| parser::local_name(e.name().as_ref()) == "body")
                {
                    let head = BytesStart::new(qualified_name(body, "head"));
                    let style = BytesStart::new(qualified_name(body, "style"));
                    write(&mut writer, Event::Start(head.clone()))?;
                    write(&mut writer, Event::Start(style.clone()))?;
                    write(&mut writer, Event::Text(BytesText::new(SOURCE_CSS)))?;
                    write(&mut writer, Event::End(style.to_end()))?;
                    write(&mut writer, Event::End(head.to_end()))?;
                    styled = true;
                }
            }
            write(&mut writer, document.events[index].clone())?;
            index += 1;
        }
        output.insert(aligned.path.clone(), writer.into_inner());
    }
    Ok(output)
}

fn qualified_name(element: &BytesStart<'_>, local: &str) -> String {
    element
        .name()
        .as_ref()
        .rsplit_once(':')
        .map(|(prefix, _)| format!("{prefix}:{local}"))
        .unwrap_or_else(|| local.to_string())
}

fn write(writer: &mut Writer<Vec<u8>>, event: Event<'_>) -> Result<(), String> {
    writer
        .write_event(event)
        .map_err(|e| format!("failed to write bilingual EPUB: {e}"))
}

fn write_target_extras(
    document: &Document,
    mut index: usize,
    end: usize,
    writer: &mut Writer<Vec<u8>>,
) -> Result<(), String> {
    while index < end {
        if let Some(element) = document.start(index) {
            let name = parser::local_name(element.name().as_ref());
            let attributes = attributes(element)?;
            let reference = name == "a"
                && attributes
                    .iter()
                    .any(|(key, value)| key == "href" && value.contains('#'));
            if reference || is_resource(&name) {
                if reference {
                    write(writer, Event::Text(BytesText::new(" ")))?;
                }
                for event in &document.events[index..=document.ends[&index]] {
                    write(writer, event.clone())?;
                }
                index = document.ends[&index] + 1;
                continue;
            }
            let mut anchor = BytesStart::new(qualified_name(element, "a"));
            for (key, value) in attributes.iter().filter(|(key, _)| is_anchor(key)) {
                anchor.push_attribute((key.as_str(), value.as_str()));
            }
            if anchor.attributes().next().is_some() {
                write(writer, Event::Empty(anchor))?;
            }
        }
        index += 1;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn write_source(
    document: &Document,
    mut index: usize,
    end: usize,
    path: &str,
    mapping: &AnchorMap,
    anchors: &HashMap<String, HashSet<String>>,
    resources: &HashSet<String>,
    writer: &mut Writer<Vec<u8>>,
) -> Result<(), String> {
    while index < end {
        if let Some(element) = document.start(index) {
            if is_resource(&parser::local_name(element.name().as_ref())) {
                index = document.ends[&index] + 1;
                continue;
            }
            let mut copy = BytesStart::new(element.name().as_ref().to_string());
            for (key, mut value) in attributes(element)? {
                if matches!(key.as_str(), "class" | "style") {
                    continue;
                }
                if is_anchor(&key) {
                    value = mapping[path][&value].clone();
                }
                if key == "href" {
                    value = source_href(path, &value, mapping, anchors, resources)?;
                }
                copy.push_attribute((key.as_str(), value.as_str()));
            }
            write(
                writer,
                if matches!(document.events[index], Event::Empty(_)) {
                    Event::Empty(copy)
                } else {
                    Event::Start(copy)
                },
            )?;
        } else if let Event::CData(text) = &document.events[index] {
            // 一些阅读器把 XHTML 交给 HTML 引擎，转义文本可避免 CDATA 被当成标签。
            write(writer, Event::Text(BytesText::new(text.as_ref())))?;
        } else {
            write(writer, document.events[index].clone())?;
        }
        index += 1;
    }
    Ok(())
}

fn source_href(
    path: &str,
    href: &str,
    mapping: &AnchorMap,
    anchors: &HashMap<String, HashSet<String>>,
    resources: &HashSet<String>,
) -> Result<String, String> {
    let base =
        url::Url::parse(&format!("https://epub.invalid/{path}")).map_err(|e| e.to_string())?;
    let target = base
        .join(href)
        .map_err(|e| format!("invalid EPUB link {href:?}: {e}"))?;
    if target.origin() != base.origin() {
        return Ok(href.to_string());
    }
    let target_path = decode_uri(target.path().trim_start_matches('/'))?;
    if !resources.contains(&target_path) {
        return Err(format!(
            "EPUB link {href:?} in {path} points to a missing resource"
        ));
    }
    let Some(fragment) = target.fragment().filter(|fragment| !fragment.is_empty()) else {
        return Ok(href.to_string());
    };
    let id = decode_uri(fragment)?;
    if let Some(copy) = mapping.get(&target_path).and_then(|m| m.get(&id)) {
        return Ok(format!(
            "{}#{copy}",
            href.split('#').next().unwrap_or_default()
        ));
    }
    if !anchors
        .get(&target_path)
        .is_some_and(|ids| ids.contains(&id))
    {
        return Err(format!(
            "EPUB link {href:?} in {path} points to missing anchor {id:?}"
        ));
    }
    Ok(href.to_string())
}

fn decode_uri(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut result = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex =
                std::str::from_utf8(&bytes[index + 1..index + 3]).map_err(|e| e.to_string())?;
            result.push(
                u8::from_str_radix(hex, 16).map_err(|e| format!("invalid EPUB URI escape: {e}"))?,
            );
            index += 3;
        } else {
            result.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(result).map_err(|e| format!("invalid EPUB URI: {e}"))
}
