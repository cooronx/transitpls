use crate::model::{Chapter, ItemStatus, SegmentKind};
use crate::parser;
use crate::state::ExportSnapshot;
use quick_xml::escape::escape;
use quick_xml::events::{BytesText, Event};
use quick_xml::{Reader, Writer, XmlVersion};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

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

pub fn render_epub(snapshot: &ExportSnapshot) -> Result<Vec<u8>, String> {
    validate_snapshot(snapshot)?;
    let source_is_epub = Path::new(&snapshot.project.source_file)
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("epub"));
    if source_is_epub {
        refill_epub(snapshot)
    } else {
        generate_epub(snapshot)
    }
}

#[derive(Debug, Clone)]
struct ManifestItem {
    href: String,
    media_type: String,
    properties: String,
}

#[derive(Debug)]
struct EpubPackage {
    opf_path: String,
    manifest: HashMap<String, ManifestItem>,
    spine: Vec<String>,
}

fn refill_epub(snapshot: &ExportSnapshot) -> Result<Vec<u8>, String> {
    let mut source = ZipArchive::new(Cursor::new(snapshot.source_bytes.as_slice()))
        .map_err(|error| format!("invalid source EPUB: {error}"))?;
    let package = read_epub_package(&mut source)?;
    let opf_dir = Path::new(&package.opf_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let mut chapter_paths = Vec::new();
    for idref in &package.spine {
        let Some(item) = package.manifest.get(idref) else {
            continue;
        };
        if !is_xhtml(item) {
            continue;
        }
        chapter_paths.push(parser::normalize_zip_path(
            opf_dir,
            item.href.split('#').next().unwrap_or(&item.href),
        ));
    }
    if chapter_paths.len() != snapshot.chapters.len() {
        return Err(format!(
            "EPUB spine has {} readable XHTML documents but the snapshot has {} chapters",
            chapter_paths.len(),
            snapshot.chapters.len()
        ));
    }

    let mut replacements = HashMap::<String, Vec<u8>>::new();
    let mut titles_by_path = HashMap::new();
    for ((chapter_index, path), chapter) in chapter_paths.iter().enumerate().zip(&snapshot.chapters)
    {
        let xhtml = read_entry_string(&mut source, path)?;
        let block_replacements = align_chapter(
            chapter_index,
            chapter,
            &xhtml,
            snapshot.project.max_segment_chars,
        )?;
        replacements.insert(path.clone(), rewrite_xhtml(&xhtml, &block_replacements)?);
        titles_by_path.insert(
            path.clone(),
            normalize_chinese_punctuation(
                chapter
                    .target_title
                    .as_deref()
                    .expect("validated chapter title should exist"),
            ),
        );
    }

    for item in package.manifest.values() {
        let entry_path = parser::normalize_zip_path(opf_dir, &item.href);
        if item
            .properties
            .split_whitespace()
            .any(|value| value == "nav")
        {
            let nav = read_entry_string(&mut source, &entry_path)?;
            replacements.insert(
                entry_path.clone(),
                rewrite_nav(&nav, &entry_path, &titles_by_path)?,
            );
        } else if item.media_type == "application/x-dtbncx+xml" {
            let ncx = read_entry_string(&mut source, &entry_path)?;
            replacements.insert(
                entry_path.clone(),
                rewrite_ncx(&ncx, &entry_path, &titles_by_path)?,
            );
        }
    }

    let opf = read_entry_string(&mut source, &package.opf_path)?;
    replacements.insert(
        package.opf_path.clone(),
        replace_element_text(&opf, "language", &snapshot.project.target_language)?,
    );
    copy_epub_with_replacements(source, &replacements)
}

fn read_epub_package<R: Read + std::io::Seek>(
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

fn is_xhtml(item: &ManifestItem) -> bool {
    item.media_type == "application/xhtml+xml" || item.media_type == "text/html"
}

fn align_chapter(
    chapter_index: usize,
    chapter: &Chapter,
    xhtml: &str,
    max_chars: usize,
) -> Result<HashMap<usize, String>, String> {
    let blocks = parser::parse_xhtml_blocks(xhtml);
    let mut segments = chapter.segments.iter().collect::<Vec<_>>();
    segments.sort_by_key(|segment| segment.ordinal);
    let mut segment_index = 0;
    let mut replacements = HashMap::new();
    for (block_ordinal, kind, source) in blocks
        .into_iter()
        .filter(|(_, _, source)| !source.trim().is_empty())
    {
        let chunks = parser::split_long_text(&source, max_chars);
        let end = segment_index + chunks.len();
        let Some(block_segments) = segments.get(segment_index..end) else {
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
        let translated = block_segments
            .iter()
            .map(|segment| {
                normalize_chinese_punctuation(
                    segment
                        .target
                        .as_deref()
                        .expect("validated segment target should exist"),
                )
            })
            .collect::<String>();
        replacements.insert(block_ordinal, translated);
        segment_index = end;
    }
    if segment_index != segments.len() {
        return Err(format!(
            "EPUB alignment failed in chapter {chapter_index}: {} saved segments were not matched",
            segments.len() - segment_index
        ));
    }
    Ok(replacements)
}

fn rewrite_xhtml(xhtml: &str, replacements: &HashMap<usize, String>) -> Result<Vec<u8>, String> {
    let mut reader = Reader::from_str(xhtml);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buffer = Vec::new();
    let mut block_ordinal = 0;
    let mut element_stack = Vec::new();
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(event)) => {
                let is_block =
                    parser::block_kind(&parser::local_name(event.name().as_ref())).is_some();
                let replacement = is_block
                    .then(|| {
                        let ordinal = block_ordinal;
                        block_ordinal += 1;
                        replacements.get(&ordinal)
                    })
                    .flatten();
                writer
                    .write_event(Event::Start(event.into_owned()))
                    .map_err(|error| format!("failed to rewrite EPUB XHTML: {error}"))?;
                if let Some(text) = replacement {
                    writer
                        .write_event(Event::Text(BytesText::new(text)))
                        .map_err(|error| format!("failed to rewrite EPUB XHTML: {error}"))?;
                }
                element_stack.push(replacement.is_some());
            }
            Ok(Event::Text(_)) if element_stack.iter().any(|replaced| *replaced) => {}
            Ok(Event::CData(_)) if element_stack.iter().any(|replaced| *replaced) => {}
            Ok(Event::End(event)) => {
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

fn rewrite_nav(
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

fn rewrite_ncx(
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
    event: &quick_xml::events::BytesStart<'_>,
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

fn replace_element_text(xml: &str, element: &str, replacement: &str) -> Result<Vec<u8>, String> {
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

fn read_entry_string<R: Read + std::io::Seek>(
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

fn copy_epub_with_replacements(
    mut source: ZipArchive<Cursor<&[u8]>>,
    replacements: &HashMap<String, Vec<u8>>,
) -> Result<Vec<u8>, String> {
    let output = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(output);
    writer
        .start_file(
            "mimetype",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .map_err(|error| format!("failed to write EPUB mimetype: {error}"))?;
    writer
        .write_all(b"application/epub+zip")
        .map_err(|error| format!("failed to write EPUB mimetype: {error}"))?;
    for index in 0..source.len() {
        let entry = source
            .by_index(index)
            .map_err(|error| format!("failed to read source EPUB entry: {error}"))?;
        let name = entry.name().to_string();
        if name == "mimetype" {
            continue;
        }
        if let Some(contents) = replacements.get(&name) {
            let options = entry.options();
            writer
                .start_file(&name, options)
                .map_err(|error| format!("failed to replace EPUB entry '{name}': {error}"))?;
            writer
                .write_all(contents)
                .map_err(|error| format!("failed to replace EPUB entry '{name}': {error}"))?;
        } else {
            writer
                .raw_copy_file(entry)
                .map_err(|error| format!("failed to copy EPUB entry '{name}': {error}"))?;
        }
    }
    writer
        .finish()
        .map(|cursor| cursor.into_inner())
        .map_err(|error| format!("failed to finish EPUB: {error}"))
}

fn generate_epub(snapshot: &ExportSnapshot) -> Result<Vec<u8>, String> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(
            "mimetype",
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )
        .map_err(|error| format!("failed to create EPUB: {error}"))?;
    writer
        .write_all(b"application/epub+zip")
        .map_err(|error| format!("failed to create EPUB: {error}"))?;
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    write_epub_entry(
        &mut writer,
        "META-INF/container.xml",
        br#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>"#,
        options,
    )?;
    let title = escape(&snapshot.project.title);
    let language = escape(&snapshot.project.target_language);
    let manifest = snapshot
        .chapters
        .iter()
        .enumerate()
        .map(|(index, _)| {
            format!(
                "    <item id=\"chapter-{0}\" href=\"chapter-{0:04}.xhtml\" media-type=\"application/xhtml+xml\"/>",
                index + 1
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let spine = snapshot
        .chapters
        .iter()
        .enumerate()
        .map(|(index, _)| format!("    <itemref idref=\"chapter-{}\"/>", index + 1))
        .collect::<Vec<_>>()
        .join("\n");
    let opf = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="book-id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="book-id">urn:sha256:{id}</dc:identifier>
    <dc:title>{title}</dc:title><dc:language>{language}</dc:language>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
{manifest}
  </manifest>
  <spine>
{spine}
  </spine>
</package>"#,
        id = snapshot.project.id
    );
    write_epub_entry(&mut writer, "OEBPS/content.opf", opf.as_bytes(), options)?;
    let nav_items = snapshot
        .chapters
        .iter()
        .enumerate()
        .map(|(index, chapter)| {
            format!(
                "      <li><a href=\"chapter-{0:04}.xhtml\">{1}</a></li>",
                index + 1,
                escape(
                    chapter
                        .target_title
                        .as_deref()
                        .expect("validated chapter title should exist")
                )
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let nav = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops" lang="{language}">
  <head><title>{title}</title></head><body><nav epub:type="toc"><ol>
{nav_items}
  </ol></nav></body>
</html>"#
    );
    write_epub_entry(&mut writer, "OEBPS/nav.xhtml", nav.as_bytes(), options)?;
    for (index, chapter) in snapshot.chapters.iter().enumerate() {
        let chapter_title = normalize_chinese_punctuation(
            chapter
                .target_title
                .as_deref()
                .expect("validated chapter title should exist"),
        );
        let skip_heading = first_matching_heading(chapter, &chapter_title);
        let paragraphs = chapter
            .segments
            .iter()
            .enumerate()
            .filter(|(segment_index, _)| Some(*segment_index) != skip_heading)
            .map(|(_, segment)| {
                let tag = match segment.kind {
                    SegmentKind::Heading => "h2",
                    SegmentKind::Quote => "blockquote",
                    SegmentKind::Paragraph | SegmentKind::Metadata => "p",
                };
                format!(
                    "    <{tag}>{}</{tag}>",
                    escape(normalize_chinese_punctuation(
                        segment
                            .target
                            .as_deref()
                            .expect("validated segment target should exist")
                    ))
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let chapter_xhtml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" lang="{language}">
  <head><title>{chapter_title}</title></head><body>
    <h1>{chapter_title}</h1>
{paragraphs}
  </body>
</html>"#,
            chapter_title = escape(&chapter_title)
        );
        write_epub_entry(
            &mut writer,
            &format!("OEBPS/chapter-{:04}.xhtml", index + 1),
            chapter_xhtml.as_bytes(),
            options,
        )?;
    }
    writer
        .finish()
        .map(|cursor| cursor.into_inner())
        .map_err(|error| format!("failed to finish EPUB: {error}"))
}

fn write_epub_entry(
    writer: &mut ZipWriter<Cursor<Vec<u8>>>,
    name: &str,
    contents: &[u8],
    options: SimpleFileOptions,
) -> Result<(), String> {
    writer
        .start_file(name, options)
        .map_err(|error| format!("failed to create EPUB entry '{name}': {error}"))?;
    writer
        .write_all(contents)
        .map_err(|error| format!("failed to write EPUB entry '{name}': {error}"))
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
    use super::{normalize_chinese_punctuation, render_epub, render_txt};
    use crate::model::{Chapter, ItemStatus, ProjectState, ProjectStatus, Segment, SegmentKind};
    use crate::state::ExportSnapshot;
    use std::io::{Cursor, Read, Write};
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipArchive, ZipWriter};

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

    #[test]
    fn epub_refill_preserves_resources_and_updates_content_and_navigation() {
        let mut snapshot = snapshot();
        snapshot.project.source_file = "book.epub".to_string();
        snapshot.chapters[0].segments[1].source = "Hello world!".to_string();
        snapshot.chapters[0].segments[1].target = Some("你好, 世界!".to_string());
        snapshot.source_bytes = source_epub();

        let bytes = render_epub(&snapshot).expect("EPUB should render");
        let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("output should be a ZIP");
        let mimetype = archive.by_index(0).expect("mimetype should be first");
        assert_eq!(mimetype.name(), "mimetype");
        assert_eq!(mimetype.compression(), CompressionMethod::Stored);
        drop(mimetype);
        assert_eq!(read_entry(&mut archive, "OEBPS/image.bin"), b"image-bytes");
        assert_eq!(read_entry(&mut archive, "OEBPS/style.css"), b"h1{}");

        let chapter = String::from_utf8(read_entry(&mut archive, "OEBPS/chapter.xhtml"))
            .expect("chapter should be UTF-8");
        assert!(chapter.contains("<h1 id=\"top\">第一章</h1>"));
        assert!(chapter.contains("<p class=\"lead\">你好， 世界！<em></em><img"));
        assert!(!chapter.contains("Hello"));
        let nav = String::from_utf8(read_entry(&mut archive, "OEBPS/nav.xhtml"))
            .expect("nav should be UTF-8");
        assert!(nav.contains(">第一章<span></span></a>"));
        let ncx = String::from_utf8(read_entry(&mut archive, "OEBPS/toc.ncx"))
            .expect("NCX should be UTF-8");
        assert!(ncx.contains("<text>第一章</text>"));
        let opf = String::from_utf8(read_entry(&mut archive, "OEBPS/content.opf"))
            .expect("OPF should be UTF-8");
        assert!(opf.contains("<dc:language>zh-CN</dc:language>"));
    }

    #[test]
    fn txt_source_generates_readable_basic_epub() {
        let snapshot = snapshot();
        let bytes = render_epub(&snapshot).expect("basic EPUB should render");
        let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("output should be a ZIP");
        assert!(archive.by_name("META-INF/container.xml").is_ok());
        let chapter = String::from_utf8(read_entry(&mut archive, "OEBPS/chapter-0001.xhtml"))
            .expect("chapter should be UTF-8");
        assert!(chapter.contains("<h1>第一章</h1>"));
        assert!(chapter.contains("<p>你好， 世界！</p>"));
        let nav = String::from_utf8(read_entry(&mut archive, "OEBPS/nav.xhtml"))
            .expect("nav should be UTF-8");
        assert!(nav.contains("chapter-0001.xhtml\">第一章</a>"));
    }

    #[test]
    fn epub_refill_rejects_source_alignment_mismatch() {
        let mut snapshot = snapshot();
        snapshot.project.source_file = "book.epub".to_string();
        snapshot.source_bytes = source_epub();
        let error = render_epub(&snapshot).expect_err("mismatched source must fail");
        assert!(error.contains("EPUB alignment failed"));
    }

    fn source_epub() -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "mimetype",
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .expect("mimetype should start");
        writer
            .write_all(b"application/epub+zip")
            .expect("mimetype should write");
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let entries: [(&str, &[u8]); 7] = [
            (
                "META-INF/container.xml",
                br#"<?xml version="1.0"?><container><rootfiles><rootfile full-path="OEBPS/content.opf"/></rootfiles></container>"#,
            ),
            (
                "OEBPS/content.opf",
                br#"<?xml version="1.0"?><package xmlns:dc="urn:dc"><metadata><dc:language>en</dc:language></metadata><manifest><item id="chapter" href="chapter.xhtml" media-type="application/xhtml+xml"/><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/><item id="css" href="style.css" media-type="text/css"/><item id="image" href="image.bin" media-type="application/octet-stream"/></manifest><spine><itemref idref="chapter"/></spine></package>"#,
            ),
            (
                "OEBPS/chapter.xhtml",
                br#"<?xml version="1.0"?><html><body><h1 id="top">Chapter 1</h1><p class="lead">Hello <em>world</em>!<img src="image.bin"/></p></body></html>"#,
            ),
            (
                "OEBPS/nav.xhtml",
                br#"<?xml version="1.0"?><html><body><nav><ol><li><a href="chapter.xhtml#top"><span>Chapter 1</span></a></li></ol></nav></body></html>"#,
            ),
            (
                "OEBPS/toc.ncx",
                br#"<?xml version="1.0"?><ncx><navMap><navPoint><navLabel><text>Chapter 1</text></navLabel><content src="chapter.xhtml#top"/></navPoint></navMap></ncx>"#,
            ),
            ("OEBPS/style.css", b"h1{}"),
            ("OEBPS/image.bin", b"image-bytes"),
        ];
        for (name, contents) in entries {
            writer
                .start_file(name, options)
                .expect("entry should start");
            writer.write_all(contents).expect("entry should write");
        }
        writer.finish().expect("EPUB should finish").into_inner()
    }

    fn read_entry<R: Read + std::io::Seek>(archive: &mut ZipArchive<R>, name: &str) -> Vec<u8> {
        let mut entry = archive.by_name(name).expect("entry should exist");
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("entry should read");
        bytes
    }
}
