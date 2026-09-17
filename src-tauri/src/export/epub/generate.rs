//! 从 TXT 快照生成基础 EPUB 3。

use super::super::first_matching_heading;
use super::super::punctuation::normalize_chinese_punctuation;
use crate::model::SegmentKind;
use crate::state::ExportSnapshot;
use quick_xml::escape::escape;
use std::io::{Cursor, Write};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

/// 生成基础 EPUB：每章一个 XHTML 文件，附 nav 目录。
pub(in crate::export) fn generate_epub(snapshot: &ExportSnapshot) -> Result<Vec<u8>, String> {
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
