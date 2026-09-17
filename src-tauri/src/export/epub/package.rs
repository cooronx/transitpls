//! EPUB 包文档解析：container.xml 与 OPF 的 manifest/spine。

use crate::parser;
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use std::collections::HashMap;
use std::io::Read;
use zip::ZipArchive;

/// OPF manifest 中的一个条目。
#[derive(Debug, Clone)]
pub(super) struct ManifestItem {
    pub(super) href: String,
    pub(super) media_type: String,
    pub(super) properties: String,
}

/// 解析出的 EPUB 包结构。
#[derive(Debug)]
pub(super) struct EpubPackage {
    pub(super) opf_path: String,
    pub(super) manifest: HashMap<String, ManifestItem>,
    pub(super) spine: Vec<String>,
}

/// 读取 container.xml 与 OPF，得到 manifest 与 spine。
pub(super) fn read_epub_package<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
) -> Result<EpubPackage, String> {
    let container = read_entry_string(archive, "META-INF/container.xml")?;
    let opf_path = rootfile_path(&container)?;
    let opf = read_entry_string(archive, &opf_path)?;
    let (manifest, spine) = package_items(&opf)?;
    Ok(EpubPackage {
        opf_path,
        manifest,
        spine,
    })
}

fn rootfile_path(xml: &str) -> Result<String, String> {
    let mut reader = Reader::from_str(xml);
    let mut buffer = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) | Ok(Event::Empty(event))
                if parser::local_name(event.name().as_ref()) == "rootfile" =>
            {
                for attribute in event.attributes().flatten() {
                    if parser::local_name(attribute.key.as_ref()) == "full-path" {
                        return attribute
                            .normalized_value(XmlVersion::Implicit1_0)
                            .map(|value| value.into_owned())
                            .map_err(|error| format!("invalid EPUB rootfile path: {error}"));
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("invalid EPUB container.xml: {error}")),
            _ => {}
        }
        buffer.clear();
    }
    Err("EPUB container does not declare a rootfile".to_string())
}

fn package_items(xml: &str) -> Result<(HashMap<String, ManifestItem>, Vec<String>), String> {
    let mut reader = Reader::from_str(xml);
    let mut buffer = Vec::new();
    let mut manifest = HashMap::new();
    let mut spine = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) | Ok(Event::Empty(event)) => {
                let name = parser::local_name(event.name().as_ref());
                if name == "item" {
                    let mut values = HashMap::new();
                    for attribute in event.attributes().flatten() {
                        if let Ok(value) = attribute.normalized_value(XmlVersion::Implicit1_0) {
                            values.insert(
                                parser::local_name(attribute.key.as_ref()),
                                value.into_owned(),
                            );
                        }
                    }
                    if let (Some(id), Some(href), Some(media_type)) = (
                        values.remove("id"),
                        values.remove("href"),
                        values.remove("media-type"),
                    ) {
                        manifest.insert(
                            id.clone(),
                            ManifestItem {
                                href,
                                media_type,
                                properties: values.remove("properties").unwrap_or_default(),
                            },
                        );
                    }
                } else if name == "itemref" {
                    for attribute in event.attributes().flatten() {
                        if parser::local_name(attribute.key.as_ref()) == "idref" {
                            if let Ok(value) = attribute.normalized_value(XmlVersion::Implicit1_0) {
                                spine.push(value.into_owned());
                            }
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("invalid EPUB package document: {error}")),
            _ => {}
        }
        buffer.clear();
    }
    Ok((manifest, spine))
}

/// 判断 manifest 条目是否为可读正文文档。
pub(super) fn is_xhtml(item: &ManifestItem) -> bool {
    item.media_type == "application/xhtml+xml" || item.media_type == "text/html"
}

/// 读取压缩包内的文本条目。
pub(super) fn read_entry_string<R: Read + std::io::Seek>(
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
