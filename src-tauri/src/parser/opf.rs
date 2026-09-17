//! EPUB 包文档（OPF/container）与导航文档（nav/NCX）解析。

use super::xhtml::{local_name, should_separate_text};
use quick_xml::escape::unescape;
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use std::collections::HashMap;

pub(super) type OpfData = (
    Option<String>,
    HashMap<String, String>,
    Vec<String>,
    Option<String>,
    Option<String>,
);

/// 导航文档中的一个目录项：目标文档路径与章节标题。
#[derive(Debug)]
struct NavigationEntry {
    href: String,
    title: String,
}

/// manifest 中声明为封面的图片项。
#[derive(Debug)]
struct CoverManifestItem {
    href: String,
    media_type: String,
    is_epub3_cover: bool,
}

/// 从 `META-INF/container.xml` 读取 OPF 文件路径。
pub(super) fn parse_rootfile_path(xml: &str) -> Result<String, String> {
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

/// 解析 OPF：书名、manifest（仅可读文档）、spine 顺序、nav 与 NCX 位置。
pub(super) fn parse_opf(xml: &str) -> Result<OpfData, String> {
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

/// 查找封面引用，优先 EPUB3 的 `cover-image` 属性，回退 EPUB2 的 `<meta name="cover">`。
pub(super) fn parse_cover_reference(xml: &str) -> Result<Option<(String, String)>, String> {
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

/// 解析 EPUB3 nav 文档中的目录项。
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

/// 解析 EPUB2 NCX 文档中的一级目录项。
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

/// 按格式选择 nav 或 NCX 解析，并过滤「目次」这类目录页标题。
pub(crate) fn parse_chapter_navigation(
    xml: &str,
    is_ncx: bool,
) -> Result<Vec<(String, String)>, String> {
    let entries = if is_ncx {
        parse_ncx_navigation(xml)?
    } else {
        parse_navigation(xml)?
    };
    Ok(entries
        .into_iter()
        .filter(|entry| !is_contents_title(&entry.title))
        .map(|entry| (entry.href, entry.title))
        .collect())
}

fn is_contents_title(title: &str) -> bool {
    matches!(title.trim(), "目次" | "Contents" | "Table of Contents")
}
