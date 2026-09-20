//! XHTML 正文块提取：识别段落级标签，拼接内联文本。

use super::txt::split_txt_paragraphs;
use crate::model::SegmentKind;
use quick_xml::escape::unescape;
use quick_xml::events::Event;
use quick_xml::Reader;

/// 按块级标签提取 XHTML 文本，返回 (序号, 类型, 文本)。
pub(crate) fn parse_xhtml_blocks(input: &str) -> Vec<(usize, SegmentKind, String)> {
    let mut reader = Reader::from_str(input);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut blocks = Vec::new();
    let mut current: Option<(usize, SegmentKind, String)> = None;
    let mut ordinal = 0;
    let mut depth: usize = 0;
    let mut block_depth = 0;
    let mut after_reference = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                depth += 1;
                let name = local_name(event.name().as_ref());
                if let Some(kind) = block_kind(&name) {
                    // q 是行内引用，不能丢掉包围它的段落文字。
                    if name != "q" || current.is_none() {
                        current = Some((ordinal, kind, String::new()));
                        block_depth = depth;
                    }
                    ordinal += 1;
                }
            }
            Ok(Event::Text(event)) => {
                if let Some((_, _, text)) = current.as_mut() {
                    let value = unescape(event.as_ref())
                        .map(|value| value.into_owned())
                        .unwrap_or_default();
                    if !after_reference && should_separate_text(text, &value) {
                        text.push(' ');
                    }
                    text.push_str(value.trim());
                    after_reference = false;
                }
            }
            Ok(Event::CData(event)) => {
                if let Some((_, _, text)) = current.as_mut() {
                    if should_separate_text(text, event.as_ref()) {
                        text.push(' ');
                    }
                    text.push_str(event.as_ref());
                }
            }
            Ok(Event::GeneralRef(event)) => {
                if let Some((_, _, text)) = current.as_mut() {
                    if let Ok(value) = unescape(&format!("&{};", event.as_ref())) {
                        text.push_str(&value);
                    }
                    after_reference = true;
                }
            }
            Ok(Event::End(event)) => {
                let name = local_name(event.name().as_ref());
                if block_kind(&name).is_some() && depth == block_depth {
                    if let Some(block) = current.take() {
                        blocks.push(block);
                    }
                }
                depth = depth.saturating_sub(1);
            }
            Ok(Event::Eof) => break,
            Err(_) => return fallback_xhtml_blocks(input),
            _ => {}
        }
        buffer.clear();
    }
    blocks
}

/// 判断两段内联文本之间是否需要补空格（避免 `Hello<em>world</em>` 粘连，也避免标点前多空格）。
pub(super) fn should_separate_text(existing: &str, incoming: &str) -> bool {
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

/// XML 解析失败时的兜底：直接剥离标签后按空行分段。
pub(super) fn fallback_xhtml_blocks(input: &str) -> Vec<(usize, SegmentKind, String)> {
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

/// 块级标签到段落类型的映射。
pub(crate) fn block_kind(name: &str) -> Option<SegmentKind> {
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Some(SegmentKind::Heading),
        "blockquote" | "q" => Some(SegmentKind::Quote),
        "p" | "li" | "pre" | "div" => Some(SegmentKind::Paragraph),
        _ => None,
    }
}

/// 去掉 XML 命名空间前缀并转小写。
pub(crate) fn local_name(value: &str) -> String {
    value
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
}
